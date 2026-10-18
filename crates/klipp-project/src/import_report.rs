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

/// Only formats already supported by the existing importers; conversion is
/// deliberately not part of reporting.
pub fn load(path: &Path, fps: klipp_core::Fps, pool: &klipp_media::MediaPool) -> Result<ImportResult, String> {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "otio" => crate::otio_import::load_otio(path).map_err(|e| e.to_string()),
        "fcpxml" => crate::fcpxml_import::load_fcpxml(path).map_err(|e| e.to_string()),
        "kdenlive" | "mlt" => crate::mlt_import::load_kdenlive(path).map_err(|e| e.to_string()),
        "edl" => {
            let bytes = crate::path_util::read_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES).map_err(|e| e.to_string())?;
            let edl = crate::edl_import::parse(&String::from_utf8_lossy(&bytes), fps);
            let name = edl.title.clone().unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            let (sequence, warnings) = crate::edl_import::build(&edl, &name, fps, |e| crate::edl_import::match_in_pool(pool, e, fps));
            Ok(ImportResult { sequence, warnings, media_pool: klipp_media::MediaPool::new() })
        }
        _ => Err("Import a .otio, .fcpxml, .kdenlive, .mlt or .edl timeline".into()),
    }
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
