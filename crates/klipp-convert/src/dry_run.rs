//! The dry run: everything the one-time conversion will do to a project,
//! done in memory and checked, with nothing written (#1066).
//!
//! Per project it reads each file with the frozen reader (and, through the
//! caller's `parity`, compares that with today's reader), converts every
//! picture store, checks both properties of the structural conversion,
//! writes the old fields and the revision on a copy and reads them back,
//! and notes a project whose picture lives on tracks or grade groups with
//! no clip to carry the sentinel. Any failure is a stop, with its reason;
//! the project's time is measured.
//!
//! `parity` is passed in so this crate never links the app's crates: the
//! dry-run example supplies it from the parity test, which may.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{assemble, lift, revision, splice, to_v2, verify, Document, StoreKind};
use klipp_graph_format::lifted::{ClipPicture, LiftNote};

/// A file compared between the frozen reader and today's reader: `Err`
/// says where they differ.
pub type Parity<'a> = &'a dyn Fn(&Value, StoreKind) -> Result<(), String>;

/// What a clip's render copy takes from its track and grade group,
/// decided with today's code (`wrap`, `touches_picture`, the merges'
/// refusals): given the clip, its sequence and its track.
pub type SpliceDecider<'a> = &'a dyn Fn(&Value, &Value, &Value) -> Result<splice::SpliceInputs, String>;

/// What the dry run found in one project.
#[derive(Debug, Default)]
pub struct ProjectReport {
    pub dir: PathBuf,
    /// Stores checked, by kind: clips, tracks, timeline grades, group
    /// stages, grade versions, grade stills, looks.
    pub stores: BTreeMap<&'static str, usize>,
    /// Why the conversion would stop, each with where.
    pub stops: Vec<String>,
    /// Picture data on tracks or grade groups but no clip: an old build
    /// from before the revision check can't be kept out of it.
    pub picture_without_clips: bool,
    /// What the lift reported, by kind of note (#1068).
    pub lift_notes: BTreeMap<String, usize>,
    /// Counts added after the store walk (revision-2 files proven).
    pub stores_extra: BTreeMap<&'static str, usize>,
    pub elapsed: Duration,
}

impl ProjectReport {
    pub fn passed(&self) -> bool {
        self.stops.is_empty()
    }
}

/// The sequences of a project file, old single-sequence files included.
fn sequences(raw: &Value) -> Vec<&Value> {
    raw.get("sequences")
        .and_then(Value::as_array)
        .map(|s| s.iter().collect())
        .or_else(|| raw.get("sequence").map(|s| vec![s]))
        .unwrap_or_default()
}

fn items<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn non_null<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|g| !g.is_null())
}

/// Whether a track's effects hold picture data: a video effect or a grade.
fn track_has_picture(track: &Value) -> bool {
    let Some(e) = non_null(track, "effects") else { return false };
    items(e, "video").next().is_some() || non_null(e, "grade").is_some()
}

/// Run the dry run over one project directory.
pub fn check_project(dir: &Path, parity: Parity) -> ProjectReport {
    check_project_with(dir, parity, None)
}

/// [`check_project`], and with `splice` also both properties of the splice
/// on every clip: `unsplice(splice(c)) == c` and `splice(splice(c)) ==
/// splice(c)`, with the clip's real track and grade group.
pub fn check_project_with(dir: &Path, parity: Parity, splice: Option<SpliceDecider>) -> ProjectReport {
    let started = Instant::now();
    let mut report = ProjectReport { dir: dir.to_path_buf(), ..Default::default() };
    check_into(dir, parity, splice, &mut report);
    report.elapsed = started.elapsed();
    report
}

fn check_into(dir: &Path, parity: Parity, splicer: Option<SpliceDecider>, report: &mut ProjectReport) {
    let text = match std::fs::read_to_string(dir.join("project.json")) {
        Ok(t) => t,
        Err(e) => return report.stops.push(format!("project.json unreadable: {e}")),
    };
    if let Err(e) = Document::parse(&text, StoreKind::Project) {
        return report.stops.push(format!("project.json: the frozen reader refuses it: {e}"));
    }
    let raw: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return report.stops.push(format!("project.json: {e}")),
    };
    if let Err(e) = parity(&raw, StoreKind::Project) {
        return report.stops.push(format!("project.json: the frozen reader differs from today's: {e}"));
    }
    match revision::read_revision(&raw) {
        Ok(0) => {}
        Ok(r) => return report.stops.push(format!("project.json: already at format revision {r}")),
        Err(e) => return report.stops.push(format!("project.json: {e}")),
    }

    let mut stop = |stores: &mut BTreeMap<&'static str, usize>, what: &'static str, at: String, r: Result<(), String>| {
        *stores.entry(what).or_default() += 1;
        if let Err(e) = r {
            report.stops.push(format!("{at}: {e}"));
        }
    };
    let mut stores = BTreeMap::new();
    let mut lift_notes: BTreeMap<String, usize> = BTreeMap::new();
    let mut took = 0usize;
    let (mut clips, mut track_or_group_picture) = (0usize, false);
    for (si, seq) in sequences(&raw).into_iter().enumerate() {
        if let Some(g) = non_null(seq, "timeline_graph") {
            track_or_group_picture = true;
            stop(&mut stores, "timeline grades", format!("sequence {si} timeline grade"), verify::check_grade(g));
        }
        for (gi, group) in items(seq, "grade_groups").enumerate() {
            for stage in ["pre", "post"] {
                if let Some(g) = non_null(group, stage) {
                    track_or_group_picture = true;
                    stop(&mut stores, "group stages", format!("sequence {si} group {gi} {stage}"), verify::check_grade(g));
                }
            }
        }
        for (ti, track) in items(seq, "tracks").enumerate() {
            track_or_group_picture |= track_has_picture(track);
            stop(&mut stores, "tracks", format!("sequence {si} track {ti}"), verify::check_track(track));
            for (ci, clip) in items(track, "clips").enumerate() {
                clips += 1;
                let at = format!("sequence {si} track {ti} clip {ci}");
                stop(&mut stores, "clips", at.clone(), verify::check_clip(clip));
                let lifted = lift_one(clip).map(|(picture, notes)| {
                    for n in notes {
                        *lift_notes.entry(note_kind(&n)).or_default() += 1;
                    }
                    picture
                });
                if let (Some(decide), Ok((picture, kind))) = (splicer, &lifted) {
                    let r = decide(clip, seq, track).and_then(|inputs| {
                        splice_properties(picture, *kind, &inputs)?;
                        if splice::splice(picture, *kind, &inputs) != *picture {
                            took += 1;
                        }
                        Ok(())
                    });
                    stop(&mut stores, "spliced clips", at.clone(), r);
                }
                let lifted = lifted.map(drop);
                stop(&mut stores, "lifted clips", at.clone(), lifted.map_err(|e| format!("the lift refused it: {e}")));
                for (vi, version) in items(clip, "grade_versions").enumerate() {
                    if let Some(g) = version.get("node_graph") {
                        stop(&mut stores, "grade versions", format!("{at} version {vi}"), verify::check_grade(g));
                    }
                }
            }
        }
    }
    report.picture_without_clips = clips == 0 && track_or_group_picture;
    // A project with no clip carries no sentinel, so a build older than the
    // revision check opens it, and its pre-#993 code could prune the
    // project's Looks or rewrite its stills: such a project stops.
    if clips == 0 && (dir.join("looks").is_dir() || dir.join("media/stills.json").is_file()) {
        report.stops.push("no clip to carry the sentinel, and Looks or stills an old build could lose".into());
    }
    report.lift_notes = lift_notes;
    if took > 0 {
        stores.insert("clips taking a splice", took);
    }

    if let Err(e) = old_fields_round_trip(&raw) {
        report.stops.push(format!("old fields: {e}"));
    }
    // Revision 2, the switch's files: built, written, proven.
    match crate::revision2::project(&raw) {
        Ok((out, _)) => {
            let r = serde_json::to_string(&out).map_err(|e| e.to_string()).and_then(|text| crate::revision2::verify_project(&raw, &text));
            match r {
                Ok(()) => *report.stores_extra.entry("revision 2 projects").or_default() += 1,
                Err(e) => report.stops.push(format!("revision 2: {e}")),
            }
            match paths_resolve(&out) {
                Ok(n) => *report.stores_extra.entry("node paths resolved").or_default() += n,
                Err(e) => report.stops.push(format!("node paths: {e}")),
            }
        }
        Err(e) => report.stops.push(format!("revision 2: {e}")),
    }
    side_stores(dir, parity, &mut stores, &mut report.stops, &mut report.lift_notes);
    report.stores = stores;
}

/// Every node of every grade in a converted project, addressed by its
/// path and resolved back to itself: clips' Grade groups (one level of
/// nesting), stages, timeline grades, track grades and track effects.
fn paths_resolve(converted: &Value) -> Result<usize, String> {
    use crate::paths::{resolve, Documents};
    use klipp_graph_format::lifted::types as lt;
    use klipp_graph_format::path::{NodePath, Root, TrackPart};
    let d = Documents::of(converted)?;
    let mut n = 0;
    let mut check = |p: NodePath| -> Result<(), String> {
        let want = p.node();
        let got = resolve(&d, &p).map_err(|e| format!("{p:?}: {e}"))?.id;
        if Some(got) != want {
            return Err(format!("{p:?} resolved to {got}"));
        }
        n += 1;
        Ok(())
    };
    for (clip, picture) in &d.clips {
        for g in picture.graph.nodes.iter().filter(|x| x.type_id.as_str() == lt::GROUP) {
            for inner in &g.group.as_ref().ok_or("a group without a body")?.graph.nodes {
                check(NodePath::new(Root::Clip { clip: *clip }, [g.id, inner.id]))?;
            }
        }
    }
    for ((group, stage), body) in &d.stages {
        for x in &body.nodes {
            check(NodePath::new(Root::GroupStage { group: *group, stage: *stage }, [x.id]))?;
        }
    }
    for (seq, body) in &d.timeline {
        for x in &body.nodes {
            check(NodePath::new(Root::TimelineGrade { sequence: *seq }, [x.id]))?;
        }
    }
    for (track, body) in &d.track_grades {
        for x in &body.nodes {
            check(NodePath::new(Root::Track { track: *track, part: TrackPart::Grade }, [x.id]))?;
        }
    }
    for (track, effects) in &d.track_effects {
        for x in effects {
            check(NodePath::new(Root::Track { track: *track, part: TrackPart::Effects }, [x.id]))?;
        }
    }
    Ok(n)
}

/// Lift one clip: its picture and kind, and its notes.
#[allow(clippy::type_complexity)]
fn lift_one(clip: &Value) -> Result<((ClipPicture, lift::ClipKind), Vec<LiftNote>), String> {
    let o = clip.as_object().ok_or("a clip isn't an object")?;
    let record = to_v2::clip_to_v2(o).map_err(|e| e.to_string())?;
    let clip_id = uuid::Uuid::parse_str(o.get("id").and_then(Value::as_str).unwrap_or_default()).map_err(|e| e.to_string())?;
    let kind = lift::ClipKind::from_json(o.get("kind"))?;
    let (p, notes) = lift::lift_clip(&record, &lift::LiftCtx { clip_id, kind })?;
    p.graph.check_lifted(false).map_err(|e| format!("the lifted graph isn't valid: {e}"))?;
    Ok(((p, kind), notes))
}

/// Both properties of the splice on one clip, and the spliced copy valid.
fn splice_properties(picture: &ClipPicture, kind: lift::ClipKind, inputs: &splice::SpliceInputs) -> Result<(), String> {
    let spliced = splice::splice(picture, kind, inputs);
    spliced.graph.check_lifted(true).map_err(|e| format!("the spliced copy isn't valid: {e}"))?;
    if &splice::unsplice(&spliced) != picture {
        return Err("unsplice(splice(c)) isn't c".into());
    }
    if splice::splice(&spliced, kind, inputs) != spliced {
        return Err("splicing twice isn't splicing once".into());
    }
    Ok(())
}

/// A note's kind, for counting.
fn note_kind(n: &LiftNote) -> String {
    serde_json::to_value(n).ok().and_then(|v| v.get("note").and_then(Value::as_str).map(str::to_owned)).unwrap_or_default()
}

/// Build the whole converted project as the real run will write it, write
/// it to text, and prove it by reading it back.
fn old_fields_round_trip(raw: &Value) -> Result<(), String> {
    let copy = assemble::to_revision1(raw)?;
    let written = serde_json::to_string(&copy).map_err(|e| e.to_string())?;
    assemble::verify(raw, &written)
}

/// A side store's file: `Ok(None)` only when it doesn't exist; any other
/// read error is a stop, so a store never drops out of the check silently.
fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// A side store's folder listing: empty only when the folder doesn't
/// exist; an unreadable folder or entry is a stop.
fn list_optional(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut paths = entries.map(|e| e.map(|e| e.path()).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

/// The project's grade stills and Looks: parity, then both properties.
fn side_stores(dir: &Path, parity: Parity, stores: &mut BTreeMap<&'static str, usize>, stops: &mut Vec<String>, notes: &mut BTreeMap<String, usize>) {
    let stills = match read_optional(&dir.join("media/stills.json")) {
        Ok(text) => text,
        Err(e) => {
            stops.push(format!("media/stills.json: {e}"));
            None
        }
    };
    if let Some(text) = stills {
        match serde_json::from_str::<Value>(&text) {
            Err(e) => stops.push(format!("media/stills.json: {e}")),
            Ok(raw) => {
                if let Err(e) = Document::parse(&text, StoreKind::Stills).map(drop).and_then(|()| parity(&raw, StoreKind::Stills)) {
                    stops.push(format!("media/stills.json: {e}"));
                }
                for (i, entry) in raw.as_array().into_iter().flatten().enumerate() {
                    if let Some(g) = non_null(entry, "grade") {
                        *stores.entry("grade stills").or_default() += 1;
                        let rest = entry.clone();
                        let wrap = move |g: Value| {
                            let mut e = rest.clone();
                            e["grade"] = g;
                            e
                        };
                        if let Err(e) = verify::check_store("still", g, &wrap, &verify::conv_still) {
                            stops.push(format!("grade still {i}: {e}"));
                        }
                    }
                }
                let r2 = crate::revision2::stills_noted(&raw).and_then(|(c, n)| {
                    for x in &n {
                        *notes.entry(note_kind(x)).or_default() += 1;
                    }
                    let text = serde_json::to_string(&c).map_err(|e| e.to_string())?;
                    crate::revision2::verify_side(&crate::revision2::stills(&raw)?, &text)
                });
                if let Err(e) = r2 {
                    stops.push(format!("media/stills.json revision 2: {e}"));
                }
            }
        }
    }
    let looks = list_optional(&dir.join("looks")).unwrap_or_else(|e| {
        stops.push(format!("looks/: {e}"));
        Vec::new()
    });
    for path in looks.into_iter().filter(|p| p.extension().is_some_and(|x| x == "json")) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let checked = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| {
            let raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            Document::parse(&text, StoreKind::Look)?;
            parity(&raw, StoreKind::Look)?;
            let id = raw.get("id").and_then(Value::as_str).ok_or("a Look without an id")?;
            let g = raw.get("node_graph").ok_or("a Look without a graph")?;
            verify::check_store("look", g, &verify::look(id), &verify::conv_look)?;
            let (c, n) = crate::revision2::look_noted(&raw)?;
            for x in &n {
                *notes.entry(note_kind(x)).or_default() += 1;
            }
            crate::revision2::verify_side(&crate::revision2::look(&raw)?, &serde_json::to_string(&c).map_err(|e| e.to_string())?)
        });
        *stores.entry("looks").or_default() += 1;
        if let Err(e) = checked {
            stops.push(format!("looks/{name}: {e}"));
        }
    }
}

/// The colour presets in a config folder (`presets/color/*.json`): parity,
/// then both properties. Returns how many were checked and the stops.
pub fn check_colour_presets(config_dir: &Path, parity: Parity) -> (usize, Vec<String>) {
    let (n, stops, _) = check_colour_presets_noted(config_dir, parity);
    (n, stops)
}

/// [`check_colour_presets`], and the lift notes of their revision-2 form.
pub fn check_colour_presets_noted(config_dir: &Path, parity: Parity) -> (usize, Vec<String>, BTreeMap<String, usize>) {
    let mut notes: BTreeMap<String, usize> = BTreeMap::new();
    let (mut n, mut stops) = (0, Vec::new());
    let files = list_optional(&config_dir.join("presets/color")).unwrap_or_else(|e| {
        stops.push(format!("presets/color/: {e}"));
        Vec::new()
    });
    for path in files.into_iter().filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json")) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let checked = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| {
            let raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            Document::parse(&text, StoreKind::GradePreset)?;
            parity(&raw, StoreKind::GradePreset)?;
            verify::check_store("colour preset", &raw, &|g| g, &verify::conv_preset)?;
            // Revision 2, as the real run writes the preset: built, written,
            // proven; its notes are counted with the projects'.
            let (c, n) = crate::revision2::colour_preset_noted(&name, &raw)?;
            for x in &n {
                *notes.entry(note_kind(x)).or_default() += 1;
            }
            crate::revision2::verify_side(&crate::revision2::colour_preset(&name, &raw)?, &serde_json::to_string(&c).map_err(|e| e.to_string())?)
        });
        n += 1;
        if let Err(e) = checked {
            stops.push(format!("presets/color/{name}: {e}"));
        }
    }
    (n, stops, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(_: &Value, _: StoreKind) -> Result<(), String> {
        Ok(())
    }

    fn project(dir: &Path, project: Value) {
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::write(dir.join("project.json"), project.to_string()).unwrap();
        std::fs::write(dir.join("media/manifest.json"), r#"{"version":"1.0.0","items":[],"bins":[]}"#).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-dry-run-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A colour preset is checked through revision 2 as well, and a broken
    /// one is a stop, not skipped.
    #[test]
    fn colour_presets_go_through_revision_2() {
        let dir = scratch("presets");
        std::fs::create_dir_all(dir.join("presets/color")).unwrap();
        let s = "00000000-0000-0000-0000-0000000000a0";
        let o = "00000000-0000-0000-0000-0000000000a1";
        let g = json!({"nodes": [{"id": s, "kind": "Source", "label": "S", "enabled": true}, {"id": o, "kind": "Output", "label": "O", "enabled": true}],
                       "edges": [{"from_node": s, "from_port": 0, "to_node": o, "to_port": 0}], "output_node_id": o});
        std::fs::write(dir.join("presets/color/Plain.json"), g.to_string()).unwrap();
        let (n, stops, notes) = check_colour_presets_noted(&dir, &ok);
        assert_eq!((n, stops.len()), (1, 0), "{stops:?}");
        assert!(notes.is_empty(), "a plain preset gives no notes");
        // A correction with a matte on port 1 and no picture: its edge is
        // parked, and the note reaches the run's count.
        let (c, k) = ("00000000-0000-0000-0000-0000000000a2", "00000000-0000-0000-0000-0000000000a3");
        let parked = json!({"nodes": [
                {"id": s, "kind": "Source", "label": "S", "enabled": true}, {"id": c, "kind": "Corrector", "label": "C", "enabled": true},
                {"id": k, "kind": "Qualifier", "label": "Q", "enabled": true}, {"id": o, "kind": "Output", "label": "O", "enabled": true}],
            "edges": [{"from_node": k, "from_port": 0, "to_node": c, "to_port": 1}, {"from_node": c, "from_port": 0, "to_node": o, "to_port": 0}],
            "output_node_id": o});
        std::fs::write(dir.join("presets/color/Parked.json"), parked.to_string()).unwrap();
        let (n, stops, notes) = check_colour_presets_noted(&dir, &ok);
        assert_eq!((n, stops.len()), (2, 0), "{stops:?}");
        assert_eq!(notes.get("parked_edge"), Some(&1), "{notes:?}");
        std::fs::remove_file(dir.join("presets/color/Parked.json")).unwrap();
        std::fs::write(dir.join("presets/color/Broken.json"), "{ not json").unwrap();
        let (n, stops, _) = check_colour_presets_noted(&dir, &ok);
        assert_eq!((n, stops.len()), (2, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_repository_fixture_passes() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-project/tests/fixtures/pre_layer_fx");
        let r = check_project(&fixture, &ok);
        assert!(r.passed(), "{:?}", r.stops);
        assert!(r.stores["clips"] > 0);
        assert!(!r.picture_without_clips);
    }

    #[test]
    fn a_project_with_track_picture_and_no_clip_is_noted() {
        let dir = scratch("no-clip");
        let crop = json!({"type": "crop", "left": 0.1, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0});
        project(&dir, json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000001", "name": "S",
            "tracks": [{"id": "00000000-0000-0000-0000-000000000002", "name": "V1", "kind": "Video", "clips": [],
                        "effects": {"video": [crop]}}]}]}));
        let r = check_project(&dir, &ok);
        assert!(r.picture_without_clips);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_name_their_reason() {
        let missing = check_project(Path::new("/nonexistent/klipp-project"), &ok);
        assert!(missing.stops[0].starts_with("project.json unreadable"));

        let dir = scratch("parity");
        project(&dir, json!({"version": "1.0.0", "sequences": []}));
        let differs = check_project(&dir, &|_: &Value, _: StoreKind| Err("clip 0 exposure".into()));
        assert!(differs.stops[0].contains("differs from today's: clip 0 exposure"), "{:?}", differs.stops);

        project(&dir, json!({"version": "1.0.0", "format_revision": 1, "sequences": []}));
        let converted = check_project(&dir, &ok);
        // The frozen reader refuses a converted file before anything else.
        assert!(converted.stops[0].contains("frozen reader refuses it"), "{:?}", converted.stops);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
