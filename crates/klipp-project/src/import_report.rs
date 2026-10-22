//! A durable, informational summary of an interchange import.
use std::{collections::BTreeMap, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use klipp_timeline::ClipKind;
use crate::mlt_import::ImportResult;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedCounts {
    pub sequences: usize,
    pub tracks: usize,
    pub clips: usize,
    pub transitions: usize,
    pub markers: usize,
    pub titles: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarningGroup {
    pub kind: String,
    /// Counts notices, not affected objects: an importer may describe several
    /// effects in one notice. Keep its exact wording rather than invent counts.
    pub warning_count: usize,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingMedia {
    pub id: klipp_core::MediaId,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    pub source: PathBuf,
    pub imported: ImportedCounts,
    pub warnings: Vec<WarningGroup>,
    pub details: Vec<String>,
    pub missing_media: Vec<MissingMedia>,
}

impl ImportReport {
    pub fn from_import(source: &Path, result: &ImportResult) -> Self {
        let seq = &result.sequence;
        let clips: Vec<_> = seq.tracks.iter().flat_map(|t| &t.clips).collect();
        let imported = ImportedCounts {
            sequences: 1, tracks: seq.tracks.len(),
            clips: clips.iter().filter(|c| !matches!(c.kind, ClipKind::Gap)).count(),
            transitions: seq.tracks.iter().map(|t| t.transitions.len()).sum(),
            markers: seq.markers.len() + clips.iter().map(|c| c.clip_markers.len()).sum::<usize>(),
            titles: clips.iter().filter(|c| matches!(c.kind, ClipKind::Title(_))).count(),
        };
        let mut groups = BTreeMap::<&str, Vec<String>>::new();
        for warning in &result.warnings {
            let lower = warning.to_lowercase();
            let kind = if lower.contains("offline") || lower.contains("not found") || lower.contains("no media") { "Missing media" }
                else if lower.contains("transition") { "Transitions" }
                else if lower.contains("effect") || lower.contains("filter") { "Effects" }
                else if lower.contains("grade") || lower.contains("colour") || lower.contains("color") { "Colour grades" }
                else if lower.contains("title") { "Titles" }
                else if lower.contains("marker") { "Markers" }
                else if lower.contains("speed") || lower.contains("timewarp") { "Speed" }
                else { "Other" };
            groups.entry(kind).or_default().push(warning.clone());
        }
        let warnings = groups.into_iter().map(|(kind, notices)| WarningGroup {
            kind: kind.into(), warning_count: notices.len(), examples: notices.into_iter().take(3).collect(),
        }).collect();
        let mut missing_media: Vec<_> = result.media_pool.all().filter(|m| m.offline).map(|m| MissingMedia {
            id: m.id, name: m.filename.clone(), path: m.path.clone(),
        }).collect();
        missing_media.sort_by(|a,b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
        Self { source: source.into(), imported, warnings, details: result.warnings.clone(), missing_media }
    }
}

/// A timeline file's interchange format, told from what is in it (#2083).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Fcpxml,
    /// Final Cut Pro 7 XML (#2084).
    Xmeml,
    Otio,
    /// Kdenlive's project, or any MLT XML.
    Mlt,
    Edl,
    /// Advanced Authoring Format: a binary compound file, not read yet
    /// (#2086).
    Aaf,
}

/// What the formats Klipp opens are, for a refusal.
pub const READS: &str = "FCPXML (.fcpxml, .xml), Final Cut Pro 7 XML (.xml), OpenTimelineIO (.otio), Kdenlive or MLT (.kdenlive, .mlt) and CMX 3600 EDL (.edl)";

/// How far into a file its format is looked for.
const SNIFF_BYTES: u64 = 64 * 1024;

/// `path`'s format from its first bytes, whatever its extension: an XML
/// file by its root element, OTIO by its schema, an EDL by its events, an
/// AAF by its compound-file signature. `Err` names the file and says what
/// is read.
pub fn detect(path: &Path) -> Result<Format, String> {
    use std::io::Read;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let mut head = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(SNIFF_BYTES).read_to_end(&mut head)).map_err(|e| format!("{name}: {e}"))?;
    detect_bytes(&head).ok_or_else(|| format!("{name} is not a timeline Klipp can open; it opens {READS}"))
}

/// [`detect`] on a file's first bytes.
pub fn detect_bytes(head: &[u8]) -> Option<Format> {
    // A compound file (AAF, and others built on it): its signature.
    if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Some(Format::Aaf);
    }
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with('{') {
        return text.contains("\"OTIO_SCHEMA\"").then_some(Format::Otio);
    }
    if text.starts_with('<') {
        // The root element: the first tag that is not a declaration, a
        // comment or a doctype.
        let root = text.match_indices('<').map(|(i, _)| &text[i + 1..]).find(|t| !t.starts_with('?') && !t.starts_with('!'))?;
        let tag: String = root.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
        return match tag.as_str() {
            "fcpxml" => Some(Format::Fcpxml),
            "xmeml" => Some(Format::Xmeml),
            "mlt" => Some(Format::Mlt),
            _ => None,
        };
    }
    // An EDL: a TITLE line, or an event line (number, reel, track, edit).
    let event = |l: &str| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 4 && f[0].len() >= 3 && f[0].chars().all(|c| c.is_ascii_digit()) && matches!(f[2].chars().next(), Some('V' | 'A' | 'B'))
    };
    text.lines().take(50).any(|l| l.trim_start().starts_with("TITLE:") || event(l)).then_some(Format::Edl)
}

/// The timeline at `path`, read by the importer for its format, told from
/// its content (#2083): a misnamed file opens as what it is. A format told
/// apart but not read yet is refused by name, with what to export instead.
pub fn load(path: &Path, fps: klipp_core::Fps, pool: &klipp_media::MediaPool) -> Result<ImportResult, String> {
    match detect(path)? {
        Format::Otio => crate::otio_import::load_otio(path).map_err(|e| e.to_string()),
        Format::Fcpxml => crate::fcpxml_import::load_fcpxml(path).map_err(|e| e.to_string()),
        Format::Mlt => crate::mlt_import::load_kdenlive(path).map_err(|e| e.to_string()),
        Format::Xmeml => crate::xmeml_import::load_xmeml(path),
        Format::Aaf => Err("AAF is not read yet; export OpenTimelineIO, FCPXML or an EDL from the other editor".into()),
        Format::Edl => {
            let bytes = crate::path_util::read_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES).map_err(|e| e.to_string())?;
            let edl = crate::edl_import::parse(&String::from_utf8_lossy(&bytes), fps);
            let name = edl.title.clone().unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            let (sequence, warnings) = crate::edl_import::build(&edl, &name, fps, |e| crate::edl_import::match_in_pool(pool, e, fps));
            let mut media_pool = klipp_media::MediaPool::new();
            for id in sequence.tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.media_id()) {
                if let Some(item) = pool.get(&id) { media_pool.add(item.clone()); }
            }
            Ok(ImportResult { sequence, warnings, media_pool })
        }
    }
}

/// A file another editor's timeline names, as a media item: what the
/// timeline says of it, probed for the rest once it is found (#2084).
pub(crate) fn interchange_media_item(path: std::path::PathBuf, name: String, duration: klipp_core::RationalTime, fps: klipp_core::Fps) -> klipp_media::MediaItem {
    use klipp_media::{MediaItem, MediaItemKind};
    use klipp_core::{MediaId, Resolution};
    let offline = !path.exists();
    MediaItem {
        id: MediaId::new(),
        path,
        kind: MediaItemKind::File,
        filename: name,
        duration,
        fps,
        resolution: Resolution { width: 0, height: 0 },
        video_codec: String::new(),
        audio_codec: String::new(),
        has_audio: false,
        file_size_bytes: 0,
        sample_rate: None,
        audio_channels: None,
        offline,
        is_still: false,
        proxy_path: None,
        proxy_ready: false,
        default_source_in: None,
        default_source_out: None,
        rating: 0,
        tags: Vec::new(),
        user_notes: String::new(),
        bin: None,
        bin_id: None,
        parent_media_id: None,
        added_at: 0,
        fingerprint: None,
        file_modified_at: None,
        media_created_at: None,
        color_label: klipp_media::ColorLabel::default(),
        colour_space: klipp_media::ColourSpace::default(),
        log_format: klipp_core::LogFormat::default(),
        source_rotation: 0,
        camera_make: None,
        camera_model: None,
        encoder: None,
        bit_depth: None,
        video_bitrate_kbps: None,
        audio_bitrate_kbps: None,
        color_primaries: None,
        color_transfer: None,
        pixel_format: None,
        detected_log_format: klipp_core::LogFormat::default(),
        log_format_user_set: false,
        skim_file: None,
        needs_reprobe: !offline,
        waveform_data: None,
        audio_cache: None,
        shot: Default::default(),
        metadata_read: false,
        truncated_from_secs: None,
        log: Default::default(),
        synced_audio: None,
        markers: Vec::new(),
        screen_text: String::new(),
        audio_role: None,
        span: None,
    }
}

/// `keys`, an opacity from 0 to 1, as the clip's picture's opacity lane.
pub(crate) fn set_opacity_keys(clip: &mut klipp_timeline::Clip, keys: &[klipp_core::Keyframe]) -> Result<(), String> {
    use klipp_graph_format::ParamKey;
    use klipp_graph_lift::ops::GraphOp;
    let path = clip.placement_path().ok_or("no placement")?;
    let ops: Vec<GraphOp> = keys
        .iter()
        .map(|k| Ok(GraphOp::AddKeyAt { path: path.clone(), key: ParamKey::from("opacity"), keyframe: serde_json::to_value(k).map_err(|e| e.to_string())? }))
        .collect::<Result<_, String>>()?;
    clip.apply_picture_ops(&ops).map(|_| ())
}

const FILE: &str = "last_import_report.json";
/// Auxiliary project information, not a change to the timeline file format.
pub fn save(dir: &Path, report: Option<&ImportReport>) -> Result<(), String> {
    let path = dir.join(FILE);
    if let Some(report) = report {
        let bytes = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
        crate::path_util::atomic_write(&path, &bytes).map_err(|e| e.to_string())
    } else if path.exists() { std::fs::remove_file(path).map_err(|e| e.to_string()) }
    else { Ok(()) }
}
pub fn read(dir: &Path) -> Result<Option<ImportReport>, String> {
    let path = dir.join(FILE);
    if !path.exists() { return Ok(None); }
    let bytes = crate::path_util::read_capped(&path, crate::path_util::MAX_INPUT_FILE_BYTES).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map(Some).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xml_extension_and_edl_existing_offline_media_are_preserved() {
        let dir = std::env::temp_dir().join(format!("klipp-report-formats-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let xml = dir.join("timeline.xml");
        std::fs::write(&xml, r#"<fcpxml version="1.9"><resources><format id="r0" frameDuration="1/24s" width="1920" height="1080"/></resources><library><event><project name="XML"><sequence format="r0" duration="1s"><spine><gap offset="0s" duration="1s"/></spine></sequence></project></event></library></fcpxml>"#).unwrap();
        let mut pool = klipp_media::MediaPool::new();
        assert!(load(&xml, klipp_core::Fps::film_24(), &pool).is_ok());
        let mut used = klipp_media::MediaItem::synthetic_for_test(dir.join("used.mov"), None);
        used.offline = true;
        let id = used.id;
        pool.add(used);
        let mut unused = klipp_media::MediaItem::synthetic_for_test(dir.join("unused.mov"), None);
        unused.offline = true;
        pool.add(unused);
        let edl = dir.join("timeline.edl");
        std::fs::write(&edl, "TITLE: EDL\n001 used V C 00:00:00:00 00:00:01:00 00:00:00:00 00:00:01:00\n* FROM CLIP NAME: used.mov\n").unwrap();
        let imported = load(&edl, klipp_core::Fps::film_24(), &pool).unwrap();
        let report = ImportReport::from_import(&edl, &imported);
        assert_eq!(report.imported.clips, 1);
        assert_eq!(report.missing_media.len(), 1);
        assert_eq!(report.missing_media[0].id, id);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #2083: each format is told from what is in the file, a misnamed one
    /// too; one not read is refused by name with what is.
    #[test]
    fn formats_are_told_from_their_content() {
        let fcpxml = br#"<?xml version="1.0"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.9"></fcpxml>"#;
        assert_eq!(detect_bytes(fcpxml), Some(Format::Fcpxml));
        assert_eq!(detect_bytes(b"<?xml version=\"1.0\"?><!-- made by hand --><xmeml version=\"5\"></xmeml>"), Some(Format::Xmeml));
        assert_eq!(detect_bytes(b"\xef\xbb\xbf<mlt LC_NUMERIC=\"C\"></mlt>"), Some(Format::Mlt));
        assert_eq!(detect_bytes(include_bytes!("../tests/fixtures/otio_multitrack.otio")), Some(Format::Otio));
        assert_eq!(detect_bytes(include_bytes!("../tests/fixtures/melt_two_clips.mlt")), Some(Format::Mlt));
        assert_eq!(detect_bytes(b"TITLE: Cut\nFCM: NON-DROP FRAME\n"), Some(Format::Edl));
        assert_eq!(detect_bytes(b"001  AX       V     C        00:00:00:00 00:00:01:00 01:00:00:00 01:00:01:00\n"), Some(Format::Edl));
        assert_eq!(detect_bytes(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]), Some(Format::Aaf));
        assert_eq!(detect_bytes(b"{\"name\": \"not otio\"}"), None);
        assert_eq!(detect_bytes(b"<html></html>"), None);
        assert_eq!(detect_bytes(b"just some notes\n"), None);

        let dir = std::env::temp_dir().join(format!("klipp-detect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // An OTIO file named .xml opens as OTIO.
        let misnamed = dir.join("cut.xml");
        std::fs::write(&misnamed, include_str!("../tests/fixtures/otio_multitrack.otio")).unwrap();
        assert_eq!(detect(&misnamed), Ok(Format::Otio));
        assert!(load(&misnamed, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).is_ok());
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, "a shopping list").unwrap();
        let why = detect(&notes).unwrap_err();
        assert!(why.starts_with("notes.txt is not a timeline") && why.contains("OpenTimelineIO"), "{why}");
        let xmeml = dir.join("fcp7.xml");
        std::fs::write(&xmeml, "<xmeml version=\"5\"/>").unwrap();
        assert_eq!(detect(&xmeml), Ok(Format::Xmeml), "read by its own importer");
        let aaf = dir.join("cut.aaf");
        std::fs::write(&aaf, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).unwrap();
        let why = load(&aaf, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).unwrap_err();
        assert!(why.contains("AAF") && why.contains("export"), "{why}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dropped_effects_missing_media_and_saved_report_match_the_import() {
        let dir = std::env::temp_dir().join(format!("klipp-report-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.otio");
        std::fs::write(&path, include_str!("../tests/fixtures/import_report.otio")).unwrap();
        let result = load(&path, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).unwrap();
        let report = ImportReport::from_import(&path, &result);
        assert_eq!(report.imported.clips, 1);
        assert_eq!(report.imported.tracks, 1);
        assert_eq!(report.missing_media.len(), 1);
        let effects = report.warnings.iter().find(|w| w.kind == "Effects").unwrap();
        assert_eq!(effects.warning_count, 1);
        assert!(effects.examples[0].contains("Missing shot"));
        save(&dir, Some(&report)).unwrap();
        assert_eq!(read(&dir).unwrap(), Some(report));
        save(&dir, None).unwrap();
        assert_eq!(read(&dir).unwrap(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
