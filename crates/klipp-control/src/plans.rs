//! Plans: ops resolved against the document, the cut changes they predict,
//! the project's generation, font warnings, and analysed transcripts (#826).

#[allow(unused_imports)]
use super::*;

/// `ops` resolved against the document as the socket sees it, with the
/// revision that view had.
pub(crate) fn resolve_plan(ctx: &ControlContext, ops: &[klipp_document::ops::Op]) -> Result<(klipp_document::ops::Plan, u64), String> {
    if ops.is_empty() {
        return Err("no ops given".into());
    }
    let revision = ctx.shared.revision();
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    // The engine's copy runs grade groups baked into their members; an op
    // changes a clip's own grade, and must not copy its group's into it.
    let seq = seq.with_own_grades();
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let ops = &with_templates(ops, template_dir().as_deref());
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
}

/// The user's title template folder (#1139).
pub(crate) fn template_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|c| klipp_timeline::template_dir(&c))
}

/// `ops` with each `template` op given the template its name names (#1139):
/// one saved in `dir`, else a built-in of that name (#1724); the document
/// resolves ops without reading files. Each is marked as made from its
/// template, as the Titles browser marks one. One whose name names none is
/// left without, which the document refuses.
pub(crate) fn with_templates(ops: &[klipp_document::ops::Op], dir: Option<&std::path::Path>) -> Vec<klipp_document::ops::Op> {
    use klipp_document::ops::Op;
    if !ops.iter().any(|op| matches!(op, Op::Template { .. })) {
        return ops.to_vec();
    }
    let saved = dir.map(klipp_timeline::read_templates).unwrap_or_default();
    ops.iter()
        .cloned()
        .map(|mut op| {
            if let Op::Template { name, data, .. } = &mut op {
                *data = match saved.iter().find(|t| t.name.eq_ignore_ascii_case(name.trim())) {
                    Some(t) => {
                        let mut td = t.data.clone();
                        td.template = Some(format!("user:{}", t.name));
                        Some(Box::new(td))
                    }
                    None => klipp_timeline::title_library::builtin(name).map(|t| Box::new(t.data())),
                };
            }
            op
        })
        .collect()
}

/// A title's live fields as `title_templates` lists them: each by label,
/// its kind, and its value as made.
fn listed_fields(td: &klipp_timeline::TitleData) -> Vec<serde_json::Value> {
    use klipp_core::TitleFieldValue as V;
    td.live_fields()
        .into_iter()
        .map(|(_, f, v)| match v {
            V::Text(s) => json!({ "label": f.label, "kind": "text", "value": s }),
            V::Colour([r, g, b, a]) => json!({ "label": f.label, "kind": "colour", "value": format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }),
            V::Speed(x) => json!({ "label": f.label, "kind": "speed", "value": x }),
            V::Size(x) => json!({ "label": f.label, "kind": "size", "value": x }),
        })
        .collect()
}

/// The title templates the template op places, as `title_templates`
/// answers: the built-in library by category (#1724), and those saved in
/// `dir` (#1139), each with the fields it keeps editable and their values
/// as made. A saved one of a built-in's name is the one placed.
pub(crate) fn title_templates(dir: Option<&std::path::Path>) -> serde_json::Value {
    use klipp_timeline::title_library::{CATEGORIES, TITLES};
    let saved = dir.map(klipp_timeline::read_templates).unwrap_or_default();
    let list: Vec<serde_json::Value> = saved
        .iter()
        .map(|t| json!({ "name": t.name, "category": t.category, "fields": listed_fields(&t.data) }))
        .collect();
    let builtin: Vec<serde_json::Value> = TITLES
        .iter()
        .map(|t| json!({ "name": t.name, "category": t.category, "fields": listed_fields(&t.data()) }))
        .collect();
    let by_category: Vec<String> = CATEGORIES
        .iter()
        .map(|c| format!("{c}: {}", TITLES.iter().filter(|t| t.category == *c).map(|t| t.name).collect::<Vec<_>>().join(", ")))
        .collect();
    let mut text = format!("{} built-in title templates. {}.", TITLES.len(), by_category.join("; "));
    text.push_str(&match saved.len() {
        0 => " None saved.".to_string(),
        n => format!(" {n} saved: {}.", saved.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")),
    });
    text.push_str(" Place one with the template op by name, setting its fields by label.");
    json!({ "text": text, "builtin": builtin, "templates": list })
}

/// Exact clip changes predicted by resolved cut edits. The UI applies the
/// same EditCommands; callers receive this only after its acknowledgement.
pub(crate) fn planned_cut_changes(ctx: &ControlContext, edits: &[klipp_document::history::EditCommand]) -> Result<serde_json::Value, String> {
    let before = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.with_own_grades().into_owned();
    let mut sequences = vec![before.clone()];
    let mut pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?.clone();
    let mut fps = before.fps;
    {
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for edit in edits { view.apply_forward(edit); }
    }
    let clips = |sequence: &klipp_timeline::Sequence| {
        let fps = sequence.fps.as_f64();
        sequence.tracks.iter().enumerate().flat_map(|(track, lane)| lane.clips.iter().map(move |clip| {
            let id = clip.id.0.to_string();
            let value = json!({
                "clip_id": id, "track": track,
                "timeline_in_frame": (clip.timeline_in.to_secs_f64() * fps).round() as i64,
                "timeline_out_frame": (clip.timeline_out.to_secs_f64() * fps).round() as i64,
                "source_in_secs": clip.source_in.to_secs_f64(), "source_out_secs": clip.source_out.to_secs_f64(),
                "linked_clip_id": clip.linked_clip_id.map(|id| id.0.to_string()),
            });
            (id, value)
        })).collect::<std::collections::BTreeMap<_, _>>()
    };
    let old = clips(&before);
    let new = clips(&sequences[0]);
    let ids = old.keys().chain(new.keys()).cloned().collect::<std::collections::BTreeSet<_>>();
    let mut changes = Vec::new();
    let mut shifted = std::collections::BTreeMap::<(u64, i64), usize>::new();
    let mut retracked = std::collections::BTreeMap::<(u64, u64), usize>::new();
    for id in ids {
        let prior = old.get(&id);
        let after = new.get(&id);
        if prior == after { continue; }
        if let (Some(prior), Some(after)) = (prior, after) {
            let delta = after["timeline_in_frame"].as_i64().unwrap_or(0)
                - prior["timeline_in_frame"].as_i64().unwrap_or(0);
            if delta != 0
                && prior["track"] == after["track"]
                && after["timeline_out_frame"].as_i64().unwrap_or(0)
                    - prior["timeline_out_frame"].as_i64().unwrap_or(0) == delta
                && prior["source_in_secs"] == after["source_in_secs"]
                && prior["source_out_secs"] == after["source_out_secs"]
                && prior["linked_clip_id"] == after["linked_clip_id"]
            {
                *shifted.entry((prior["track"].as_u64().unwrap_or(0), delta)).or_default() += 1;
                continue;
            }
            // A clip that only changed index because a track was added or
            // removed above it is one count per move, not a full entry
            // each (#1589).
            if prior["track"] != after["track"]
                && prior["timeline_in_frame"] == after["timeline_in_frame"]
                && prior["timeline_out_frame"] == after["timeline_out_frame"]
                && prior["source_in_secs"] == after["source_in_secs"]
                && prior["source_out_secs"] == after["source_out_secs"]
                && prior["linked_clip_id"] == after["linked_clip_id"]
            {
                *retracked.entry((prior["track"].as_u64().unwrap_or(0), after["track"].as_u64().unwrap_or(0))).or_default() += 1;
                continue;
            }
        }
        changes.push(json!({ "clip_id": id, "before": prior, "after": after,
            "created": prior.is_none(), "removed": after.is_none() }));
    }
    let shifted: Vec<_> = shifted.into_iter().map(|((track, by_frames), clips)| {
        json!({ "track": track, "clips": clips, "by_frames": by_frames })
    }).collect();
    let retracked: Vec<_> = retracked.into_iter().map(|((from_track, to_track), clips)| {
        json!({ "from_track": from_track, "to_track": to_track, "clips": clips })
    }).collect();
    Ok(json!({ "changes": changes, "shifted": shifted, "retracked": retracked }))
}

/// Every reply carries which project it is about: the generation moves
/// when a project is made, opened or saved, and a client that sees it
/// move knows the timeline and the pool it was working on are gone.
pub(crate) fn with_project_generation(ctx: &ControlContext, mut v: serde_json::Value) -> serde_json::Value {
    if let (Some(obj), Ok(p)) = (v.as_object_mut(), ctx.shared.project.lock()) {
        obj.insert("project_generation".into(), json!(p.generation));
        obj.insert("project_identity".into(), json!(p.identity));
    }
    v
}

/// A PNG's size from its header, once the file is whole (it ends in the
/// IEND chunk): a file still being written reads as `None`.
pub(crate) fn png_size_if_whole(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33 || !bytes.starts_with(SIGNATURE) || &bytes[12..16] != b"IHDR" || !bytes.ends_with(&[0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]) {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some((be(16), be(20)))
}

pub(crate) fn project_status(ctx: &ControlContext) -> Result<klipp_core::ProjectStatus, String> {
    Ok(ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?.clone())
}

/// Wait for the UI to report a save or an open: its generation moves
/// when one ends, well or badly.
pub(crate) fn wait_for_project_event(ctx: &ControlContext, before: u64, timeout_ms: u64) -> Result<klipp_core::ProjectStatus, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let now = project_status(ctx)?;
        if now.generation != before {
            return Ok(now);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("the app did not report back within {timeout_ms} ms; see the log"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A project is a folder whose name ends in `.klipp`.
pub(crate) fn with_klipp_extension(mut path: std::path::PathBuf) -> std::path::PathBuf {
    if path.extension().and_then(|e| e.to_str()) != Some("klipp") {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        path.set_file_name(format!("{name}.klipp"));
    }
    path
}

/// The transcripts analysis has made, for the resolver's `captions`.
pub(crate) struct AnalysedTranscripts;

impl klipp_document::ops::Transcripts for AnalysedTranscripts {
    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<klipp_document::ops::SpokenWord>> {
        let t = klipp_analysis::report::MediaAnalysis::load(&item.path).transcript?;
        Some(t.words.into_iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text, confidence: w.confidence }).collect())
    }

    fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).audio.map(|a| a.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default()
    }

    fn shot_changes(&self, item: &klipp_media::MediaItem) -> Vec<f64> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).shots.map(|s| s.boundaries).unwrap_or_default()
    }

    fn motion_between(&self, item: &klipp_media::MediaItem, from: f64, to: f64) -> Option<f32> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).motion?.mean_within(from, to)
    }

    fn library_role(&self, item: &klipp_media::MediaItem) -> Option<klipp_core::ClipRole> {
        match klipp_audio_search::library_source(&item.path)? {
            klipp_audio_search::Source::JamendoMusic => Some(klipp_core::ClipRole::Music),
            klipp_audio_search::Source::Freesound => Some(klipp_core::ClipRole::SFX),
        }
    }

    fn screen_sensitive(&self, item: &klipp_media::MediaItem) -> Vec<klipp_document::ops::SensitiveText> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).screentext.map(|st| st.sensitive.into_iter().map(|s| klipp_document::ops::SensitiveText { kind: s.kind, masked: s.masked, start_secs: s.start_secs, end_secs: s.end_secs, x: s.x, y: s.y, w: s.w, h: s.h }).collect()).unwrap_or_default()
    }

    fn people(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).framing.map(|f| f.samples.iter().flat_map(|s| s.people.iter().map(move |p| (s.secs, [p.0, p.1, p.2, p.3]))).collect()).unwrap_or_default()
    }

    fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).framing.map(|f| f.samples.iter().flat_map(|s| face_or_head_regions(&f, s).into_iter().map(move |b| (s.secs, b))).collect()).unwrap_or_default()
    }
    /// As the compositor places the clip, keys and picture-truth lanes
    /// included (#1909).
    fn placement(&self, clip: &klipp_timeline::Clip, clip_secs: f64, fps: f64) -> Option<klipp_timeline::surface::Placement> {
        let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(clip_secs.max(0.0), clip.timeline_in.rate.max(1));
        Some(klipp_compose::eval::evaluate_clip_own_transform(clip, clip_secs.max(0.0), &klipp_compose::eval::eval_context(clip, when, fps)))
    }

    /// The beat sheet, for a screen recording only: one made for camera
    /// footage the metadata took for a screen would hold its shots to a
    /// screen's rules (#1568).
    fn screen_beats(&self, item: &klipp_media::MediaItem) -> Option<klipp_document::ops::ScreenBeats> {
        let analysis = klipp_analysis::report::MediaAnalysis::load(&item.path);
        let bs = analysis.screen_beatsheet(item)?;
        if !bs.has_freeze {
            return None;
        }
        Some(klipp_document::ops::ScreenBeats {
            action: bs.segments.iter().filter(|s| s.kind == "action").map(|s| (s.start_secs, s.end_secs)).collect(),
            settles: bs.reveals.iter().copied().chain(bs.cut_points.iter().map(|c| c.secs)).collect(),
            holds: bs.title_windows.iter().map(|w| (w.start_secs, w.end_secs)).collect(),
            spots: bs.spots.iter().map(|s| (s.secs, s.x, s.y)).collect(),
        })
    }
}

/// Whether titles can be set in `family`: bundled with Klipp or
/// installed, matched without case (#738).
pub(crate) fn font_available(family: &str) -> bool {
    let f = family.trim();
    klipp_title::bundled::families().iter().any(|(name, _)| name.eq_ignore_ascii_case(f))
        || klipp_title::font_discovery::discover_system_fonts().keys().any(|k| k.eq_ignore_ascii_case(f))
}

/// What to tell a client that named fonts titles cannot be set in (#738):
/// a brief once took "Montserrat" without a word, and its kickers came out
/// in the fallback.
pub(crate) fn font_warnings<'a>(fonts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for f in fonts.map(str::trim).filter(|f| !f.is_empty()) {
        if !font_available(f) && !seen.iter().any(|s| s.eq_ignore_ascii_case(f)) {
            seen.push(f.to_owned());
        }
    }
    seen.into_iter()
        .map(|f| format!("font \"{f}\" is neither bundled nor installed: titles in it fall back to {}. klipp_fonts lists the fonts there are.", klipp_title::bundled::DEFAULT_FAMILY))
        .collect()
}

/// The fonts the ops in a plan name that cannot be had.
pub(crate) fn op_font_warnings(ops: &[klipp_document::ops::Op]) -> Vec<String> {
    use klipp_document::ops::Op;
    font_warnings(ops.iter().filter_map(|o| match o {
        Op::Title { font: Some(f), .. } | Op::Style { font: Some(f), .. } => Some(f.as_str()),
        _ => None,
    }))
}

/// `warnings` on a reply, and said in its text.
pub(crate) fn with_warnings(v: &mut serde_json::Value, warnings: Vec<String>) {
    if warnings.is_empty() {
        return;
    }
    if let Some(text) = v["text"].as_str() {
        v["text"] = json!(format!("{text}\n{}", warnings.iter().map(|w| format!("[warning] {w}")).collect::<Vec<_>>().join("\n")));
    }
    v["warnings"] = json!(warnings);
}

#[cfg(test)]
mod template_tests {
    use super::*;

    /// A template saved in `dir` as the Titles browser writes one.
    fn saved(dir: &std::path::Path) -> klipp_timeline::TitleData {
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Jane Doe".into(), ..Default::default() });
        td.published = td.default_fields();
        let file = json!({ "name": "Guest", "category": "Lower Thirds", "written_by": "0.1.0", "data": td });
        std::fs::write(dir.join(format!("Guest.{}", klipp_timeline::TEMPLATE_EXTENSION)), file.to_string()).unwrap();
        td
    }

    /// title_templates lists each saved template with its fields as made;
    /// a template op gets its template by name in any case, and one naming
    /// none is left without (#1139).
    #[test]
    fn templates_are_listed_and_given_to_their_ops() {
        let dir = std::env::temp_dir().join(format!("klipp-control-templates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let td = saved(&dir);
        let listed = title_templates(Some(&dir));
        assert_eq!(listed["templates"][0]["name"], "Guest");
        assert_eq!(listed["templates"][0]["fields"][0], json!({ "label": "Text 1", "kind": "text", "value": "Jane Doe" }));
        assert_eq!(listed["templates"][0]["fields"][1]["kind"], "colour");
        assert!(listed["text"].as_str().unwrap().contains("1 saved: Guest"), "{listed}");
        assert!(title_templates(None)["text"].as_str().unwrap().contains("None saved"));

        let op = |name: &str| klipp_document::ops::Op::Template { name: name.into(), at: klipp_document::ops::Anchor::Secs(0.0), secs: 3.0, track: None, fields: Default::default(), data: None };
        let filled = with_templates(&[op("guest"), op("Nobody")], Some(&dir));
        let data = |op: &klipp_document::ops::Op| match op {
            klipp_document::ops::Op::Template { data, .. } => data.as_deref().cloned(),
            _ => None,
        };
        let mut marked = td;
        marked.template = Some("user:Guest".into());
        assert_eq!(data(&filled[0]), Some(marked), "by name, any case, marked as made from it");
        assert_eq!(data(&filled[1]), None, "no such template");
        std::fs::remove_dir_all(dir).ok();
    }

    /// The built-in library is listed by category with its fields, and a
    /// template op names a built-in by name and gets it, marked as made
    /// from it; a saved template of the same name wins (#1724).
    #[test]
    fn built_in_templates_are_listed_and_placed_by_name() {
        let listed = title_templates(None);
        let builtin = listed["builtin"].as_array().unwrap();
        assert_eq!(builtin.len(), klipp_timeline::title_library::TITLES.len());
        let bar = builtin.iter().find(|t| t["name"] == "Name Bar").unwrap();
        assert_eq!(bar["category"], "Lower Thirds");
        let labels: Vec<&str> = bar["fields"].as_array().unwrap().iter().map(|f| f["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Name", "Role", "Accent", "Size", "Speed"]);
        assert!(listed["text"].as_str().unwrap().contains("Lower Thirds: Name Bar"), "{}", listed["text"]);

        let op = |name: &str| klipp_document::ops::Op::Template { name: name.into(), at: klipp_document::ops::Anchor::Secs(0.0), secs: 3.0, track: None, fields: Default::default(), data: None };
        let filled = with_templates(&[op("name bar")], None);
        let klipp_document::ops::Op::Template { data: Some(td), .. } = &filled[0] else { panic!("a built-in by name") };
        assert_eq!(td.template.as_deref(), Some("Name Bar"));
        assert_eq!(td.field_named("Role").map(|i| td.field_value(i)), Some(Some(klipp_core::TitleFieldValue::Text("[Title or role]".into()))));

        let dir = std::env::temp_dir().join(format!("klipp-control-templates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = json!({ "name": "Name Bar", "category": "Lower Thirds", "written_by": "0.1.0", "data": saved(&dir) });
        std::fs::write(dir.join(format!("Name Bar.{}", klipp_timeline::TEMPLATE_EXTENSION)), file.to_string()).unwrap();
        let filled = with_templates(&[op("Name Bar")], Some(&dir));
        let klipp_document::ops::Op::Template { data: Some(td), .. } = &filled[0] else { panic!() };
        assert_eq!(td.template.as_deref(), Some("user:Name Bar"), "the saved one wins");
        std::fs::remove_dir_all(dir).ok();
    }
}

