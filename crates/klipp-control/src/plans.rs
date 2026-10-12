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
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
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
        }
        changes.push(json!({ "clip_id": id, "before": prior, "after": after,
            "created": prior.is_none(), "removed": after.is_none() }));
    }
    let shifted: Vec<_> = shifted.into_iter().map(|((track, by_frames), clips)| {
        json!({ "track": track, "clips": clips, "by_frames": by_frames })
    }).collect();
    Ok(json!({ "changes": changes, "shifted": shifted }))
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
