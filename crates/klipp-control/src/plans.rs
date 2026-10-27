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
    let ops = &with_compositions(&with_templates(ops, template_dir().as_deref()), composition_dir().as_deref(), &project_sequence_ids(ctx, &seq));
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
}

/// [`resolve_plan`], seeing the project's other sequences as the UI last
/// published them, for `plan` and `apply`'s `edit_from_selects` (#2385).
/// The plan names each it copied from, with the content it read; the UI
/// refuses the edits if one has changed since.
pub(crate) fn resolve_plan_seeing(ctx: &ControlContext, ops: &[klipp_document::ops::Op]) -> Result<(klipp_document::ops::Plan, u64), String> {
    if !ops.iter().any(|o| matches!(o, klipp_document::ops::Op::EditFromSelects { .. })) {
        return resolve_plan(ctx, ops);
    }
    let revision = ctx.shared.revision();
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let seq = seq.with_own_grades();
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let others = ctx.others.read().map_err(|_| "sequences lock poisoned")?.clone();
    let ops = &with_compositions(&with_templates(ops, template_dir().as_deref()), composition_dir().as_deref(), &project_sequence_ids(ctx, &seq));
    let plan = klipp_document::ops::resolve_in(&seq, &others, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
}

/// The user's title template folder (#1139).
pub(crate) fn template_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|c| klipp_timeline::template_dir(&c))
}

/// The user's composition template folder (#225, #2757).
pub(crate) fn composition_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|c| klipp_timeline::composition_template::CompositionTemplateStore::default_dir(&c))
}

/// The ids of every sequence in the project: the open one and the others
/// as the UI last published them.
fn project_sequence_ids(ctx: &ControlContext, open: &klipp_timeline::Sequence) -> Vec<klipp_core::SequenceId> {
    let mut ids = vec![open.id];
    if let Ok(others) = ctx.others.read() {
        ids.extend(others.iter().map(|o| o.sequence.id));
    }
    ids
}

/// `ops` with each `template` op no title template answered given the
/// composition template of its name in `dir` (#2757), with the sequences
/// of it the project (`have`, by id) lacks, which placing it copies in.
pub(crate) fn with_compositions(ops: &[klipp_document::ops::Op], dir: Option<&std::path::Path>, have: &[klipp_core::SequenceId]) -> Vec<klipp_document::ops::Op> {
    use klipp_document::ops::{Op, PlacedComposition};
    if !ops.iter().any(|op| matches!(op, Op::Template { data: None, .. })) {
        return ops.to_vec();
    }
    let store = klipp_timeline::composition_template::CompositionTemplateStore::load(dir.map(std::path::Path::to_path_buf));
    ops.iter()
        .cloned()
        .map(|mut op| {
            if let Op::Template { name, data: None, composition, .. } = &mut op {
                *composition = store.templates().find(|t| t.name.eq_ignore_ascii_case(name.trim())).map(|t| {
                    let missing = t.sequences.iter().filter(|s| !have.contains(&s.id)).cloned().collect();
                    Box::new(PlacedComposition { template: t.clone(), missing })
                });
            }
            op
        })
        .collect()
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
            V::Font(s) => json!({ "label": f.label, "kind": "font", "value": s }),
            V::Duration(x) => json!({ "label": f.label, "kind": "duration", "value": x }),
            V::Picture(s) => json!({ "label": f.label, "kind": "picture", "value": s }),
        })
        .collect()
}

/// The title templates the template op places, as `title_templates`
/// answers: the built-in library by category (#1724), and those saved in
/// `dir` (#1139), each with the fields it keeps editable and their values
/// as made. A saved one of a built-in's name is the one placed.
pub(crate) fn title_templates(dir: Option<&std::path::Path>, compositions: Option<&std::path::Path>) -> serde_json::Value {
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
    // Composition templates (#2757): a stack of clips placed as one,
    // their parameters set by name or label.
    let store = klipp_timeline::composition_template::CompositionTemplateStore::load(compositions.map(std::path::Path::to_path_buf));
    let composed: Vec<serde_json::Value> = store
        .templates()
        .map(|t| {
            let parameters: Vec<serde_json::Value> = t.parameters.iter().map(|(name, p)| {
                let mut v = json!({ "name": name, "label": p.label, "default": p.default, "choices": p.choices });
                // A macro control (#2757): its range and what it drives.
                let drives: Vec<serde_json::Value> = t.bindings.iter().filter(|b| &b.parameter == name).filter_map(|b| b.link.as_ref().map(|l| json!({ "target": b.property, "to": l.to, "curve": l.curve.name() }))).collect();
                if !drives.is_empty() {
                    v["range"] = json!(p.range.as_ref().map(|r| [r.min.unwrap_or(0.0), r.max.unwrap_or(1.0)]));
                    v["drives"] = json!(drives);
                }
                v
            }).collect();
            let secs = t.composition().map(|s| s.total_duration().to_secs_f64()).unwrap_or(0.0);
            let slots: Vec<serde_json::Value> = t.slots.iter().map(|(name, s)| json!({ "name": name, "label": s.label, "kind": s.kind })).collect();
            json!({ "name": t.name, "category": t.category, "secs": secs, "parameters": parameters, "slots": slots })
        })
        .collect();
    if !composed.is_empty() {
        text.push_str(&format!(" {} composition templates (a stack of clips placed as one): {}.", composed.len(), store.templates().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    text.push_str(" Place one with the template op by name, setting its fields (a composition's parameters) by label.");
    json!({ "text": text, "builtin": builtin, "templates": list, "compositions": composed })
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

/// [`planned_cut_changes`]' changes as the clips' ids alone: created,
/// removed, and changed in place (#2492).
pub(crate) fn changes_by_id(changes: &serde_json::Value) -> serde_json::Value {
    let ids = |want: &dyn Fn(&serde_json::Value) -> bool| -> Vec<serde_json::Value> {
        changes.as_array().into_iter().flatten().filter(|c| want(c)).map(|c| c["clip_id"].clone()).collect()
    };
    let is = |c: &serde_json::Value, k: &str| c[k] == true;
    json!({
        "created": ids(&|c| is(c, "created")),
        "removed": ids(&|c| is(c, "removed")),
        "changed": ids(&|c| !is(c, "created") && !is(c, "removed")),
        "detail": "each clip's before and after with detail: true",
    })
}

#[cfg(test)]
mod changes_by_id_tests {
    /// #2492: without detail a cut's changes are the clips' ids.
    #[test]
    fn a_cuts_changes_are_ids_without_detail() {
        let full = serde_json::json!([
            { "clip_id": "a", "before": {}, "after": null, "created": false, "removed": true },
            { "clip_id": "b", "before": null, "after": {}, "created": true, "removed": false },
            { "clip_id": "c", "before": {}, "after": {}, "created": false, "removed": false },
        ]);
        let v = super::changes_by_id(&full);
        assert_eq!((v["removed"].clone(), v["created"].clone(), v["changed"].clone()), (serde_json::json!(["a"]), serde_json::json!(["b"]), serde_json::json!(["c"])));
    }
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

/// The installed LV2 and CLAP plug-ins as the resolver checks an edit
/// against them (#2485), from the registries the app filled at start.
/// An LV2 port that declares no bound gets 0..1, as the inspector shows.
pub(crate) fn installed_audio_plugins() -> Vec<klipp_document::ops::AudioPlugin> {
    use klipp_document::ops::{AudioPlugin, AudioPluginParam};
    let lv2 = klipp_audio::lv2::registry_snapshot().into_iter().map(|p| AudioPlugin {
        id: p.type_id(),
        name: p.name.clone(),
        params: p.control_inputs.iter().map(|c| AudioPluginParam { key: c.symbol.clone(), name: c.name.clone(), min: c.min.unwrap_or(0.0), max: c.max.unwrap_or(1.0), default: c.default }).collect(),
        cannot_run: None,
    });
    let clap = klipp_audio::clap::registry_snapshot().into_iter().map(|p| AudioPlugin {
        id: p.type_id(),
        name: p.name.clone(),
        params: p.params.iter().map(|c| AudioPluginParam { key: c.key(), name: c.name.clone(), min: c.min, max: c.max, default: c.default }).collect(),
        cannot_run: None,
    });
    lv2.chain(clap).collect()
}

impl klipp_document::ops::Transcripts for AnalysedTranscripts {
    fn audio_plugin(&self, id: &str) -> Option<klipp_document::ops::AudioPlugin> {
        // Run once before an edit puts it on: a host that refuses it would
        // pass the sound through untouched.
        installed_audio_plugins().into_iter().find(|p| p.id == id).map(|p| klipp_document::ops::AudioPlugin { cannot_run: klipp_audio::dsp::probe_run(id, 48_000).err(), ..p })
    }

    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<klipp_document::ops::SpokenWord>> {
        let t = klipp_analysis::report::MediaAnalysis::load(&item.path).transcript?;
        Some(t.words.into_iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text, confidence: w.confidence }).collect())
    }

    fn language(&self, item: &klipp_media::MediaItem) -> Option<String> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).transcript?.language
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
        match klipp_audio_search::library_source(&item.path) {
            Some(klipp_audio_search::Source::JamendoMusic) => Some(klipp_core::ClipRole::Music),
            Some(klipp_audio_search::Source::Freesound) => Some(klipp_core::ClipRole::SFX),
            // Else what its sound was surely classified as (#2216).
            None => item.audio_role.filter(|p| p.confident).map(|p| p.role),
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

    fn picture_geometry(&self, seq: &klipp_timeline::Sequence, clip: &klipp_timeline::Clip, at: f64) -> Option<klipp_document::qc_geometry::Geometry> {
        use klipp_effects::BuiltinEffect;
        let time = klipp_core::Timecode::from_secs_f64(at, clip.timeline_in.rate.max(1));
        let placement = klipp_compose::eval::placement_for_editing(clip, seq, time).ok()?;
        // Adjustment-layer masks/warps are not represented by the flat
        // placement helper. Leave those samples for the visual check.
        if let Some(index) = seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip.id)) {
            if seq.tracks[..index].iter().filter(|t|t.visible).flat_map(|t|&t.clips).any(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::AdjustmentLayer) && c.timeline_in <= time && c.timeline_out > time) {
                return None;
            }
        }
        let (effects, opacity) = klipp_compose::render::clip_effects(clip, time, seq.fps.as_f64(), None, None);
        let mut g = klipp_document::qc_geometry::Geometry { placement, opacity, ..Default::default() };
        if clip.surface_pin.is_some() || clip.matte_source.is_some() || clip.matte_stencil.is_some()
            || clip.consumed_as_matte || clip.blend_mode != klipp_core::BlendMode::Normal
            || clip.fade_in_secs > 0.0 || clip.fade_out_secs > 0.0
            || placement.is_some_and(|p| p.4.abs() > 1e-5)
        { return None; }
        // Occlusion additionally requires a known non-alpha media format
        // (checked against the pool by the document layer).
        g.opaque = matches!(clip.kind, klipp_timeline::ClipKind::Media { .. });
        for effect in effects {
            match effect {
                // Crop operates on the fitted frame and may precede or
                // follow placement. A flat placement cannot represent
                // that ordered chain, so request visual inspection.
                BuiltinEffect::Crop { .. } => return None,
                BuiltinEffect::Transform { rotation_x: 0.0, rotation_y: 0.0, focal_ref: None, .. }
                | BuiltinEffect::Opacity { .. }
                | BuiltinEffect::Lut3D { .. } | BuiltinEffect::ColourSpaceTransform { .. }
                | BuiltinEffect::Gamma { .. } | BuiltinEffect::HSVAdjust { .. }
                | BuiltinEffect::Vibrance { .. } | BuiltinEffect::Levels { .. }
                | BuiltinEffect::Invert { .. } | BuiltinEffect::ChannelMixer { .. }
                | BuiltinEffect::GradeCompound { .. } => {}
                // Do not report a definite overlap through unsupported
                // masks, warps or effects that change the visible footprint.
                _ => return None,
            }
        }
        if let Some(track) = seq.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip.id)) {
            if track.effects.touches_picture() { return None; }
            g.opacity *= track.opacity;
            // Transitions can reveal the layer below even when placement
            // is unchanged. Leave their overlap for visual inspection.
            if klipp_compose::render::active_layers(seq, time).iter().any(|layer| layer.transition && layer.clips.iter().any(|c| c.id == clip.id)) { return None; }
        }
        Some(g)
    }

    fn title_regions(&self, title: &klipp_timeline::TitleData, frame: klipp_core::Resolution, local: f64, duration: f64) -> Vec<[f32;4]> {
        // A small alpha render follows the actual text layout, alignment,
        // object keys, entrances/exits and character animation. Retain
        // separate horizontal runs so empty space between words/layers
        // does not become an opaque bounding rectangle.
        thread_local! { static TITLES: std::cell::RefCell<klipp_title::TitleRenderer> = std::cell::RefCell::new(klipp_title::TitleRenderer::new()); }
        let scale = 480.0 / frame.width.max(frame.height).max(1) as f64;
        let w = (frame.width as f64 * scale).round().max(1.0) as u32;
        let h = (frame.height as f64 * scale).round().max(1.0) as u32;
        TITLES.with(|renderer| {
            let pixels = renderer.borrow_mut().render_with_window(title,w,h,None,local as f32,Some(duration as f32));
            let mut regions = Vec::new();
            for y in 0..h {
                let mut x = 0;
                while x < w {
                    if pixels[((y*w+x)*4+3) as usize] < 16 { x+=1; continue; }
                    let start=x;
                    while x<w && pixels[((y*w+x)*4+3) as usize]>=16 {x+=1;}
                    regions.push([start as f32/w as f32,y as f32/h as f32,(x-start) as f32/w as f32,1.0/h as f32]);
                }
            }
            regions
        })
    }

    /// The bed's beats when they have been read; asking starts a read in
    /// the background for the next check, as a cut to the music does.
    /// Not read yet, the read is started and waited for, a bounded
    /// while, once per file (#1977).
    fn beats(&self, item: &klipp_media::MediaItem) -> Option<(f32, Vec<f64>)> {
        let found = crate::cuts::bed_beats(&item.path).or_else(|| {
            static WAITED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>> = std::sync::OnceLock::new();
            let first = WAITED.get_or_init(Default::default).lock().map(|mut w| w.insert(item.path.clone())).unwrap_or(false);
            first.then(|| wait_for(BEATS_WAIT, std::time::Duration::from_millis(100), || klipp_analysis::beats::cached(&item.path))).flatten()
        });
        found.map(|b| (b.confidence, b.beat_times))
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

/// How long the check waits for a bed's beats it has just started
/// reading (#1977).
const BEATS_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// `look` every `every` until it finds something or `bound` has passed.
pub(crate) fn wait_for<T>(bound: std::time::Duration, every: std::time::Duration, mut look: impl FnMut() -> Option<T>) -> Option<T> {
    let until = std::time::Instant::now() + bound;
    loop {
        if let Some(found) = look() {
            return Some(found);
        }
        if std::time::Instant::now() >= until {
            return None;
        }
        std::thread::sleep(every.min(until.saturating_duration_since(std::time::Instant::now())));
    }
}

#[cfg(test)]
mod wait_tests {
    use super::wait_for;
    use std::time::{Duration, Instant};

    /// #1977: what arrives within the bound is used; what never arrives
    /// is given up on at the bound.
    #[test]
    fn a_wait_ends_at_its_answer_or_its_bound() {
        let mut n = 0;
        assert_eq!(wait_for(Duration::from_secs(2), Duration::from_millis(1), || { n += 1; (n == 3).then_some(n) }), Some(3));
        let start = Instant::now();
        assert_eq!(wait_for::<()>(Duration::from_millis(50), Duration::from_millis(5), || None), None);
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(50) && took < Duration::from_millis(500), "{took:?}");
    }
}

/// Whether titles can be set in `family`: bundled with Klipp or
/// installed, matched without case (#738).
pub(crate) fn font_available(family: &str) -> bool {
    let f = family.trim();
    klipp_title::bundled::families().iter().any(|(name, _)| name.eq_ignore_ascii_case(f))
        || klipp_title::font_discovery::discover_system_fonts().keys().any(|k| k.eq_ignore_ascii_case(f))
        || klipp_title::catalogue::fetched(f)
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
        .map(|f| match klipp_title::catalogue::family(&f) {
            // In the catalogue: one fetch away (#2732).
            Some(c) => format!("font \"{}\" is in Klipp's font catalogue but not fetched yet: titles in it fall back to {} until it is. Fetch it with klipp_fonts {{fetch: \"{}\"}} ({:.1} MB).", c.name, klipp_title::bundled::DEFAULT_FAMILY, c.name, c.files.iter().map(|x| x.size).sum::<u64>() as f64 / 1e6),
            None => format!("font \"{f}\" is neither bundled nor installed: titles in it fall back to {}. klipp_fonts lists the fonts there are, and the catalogue's to fetch.", klipp_title::bundled::DEFAULT_FAMILY),
        })
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
        let listed = title_templates(Some(&dir), None);
        assert_eq!(listed["templates"][0]["name"], "Guest");
        assert_eq!(listed["templates"][0]["fields"][0], json!({ "label": "Text 1", "kind": "text", "value": "Jane Doe" }));
        assert_eq!(listed["templates"][0]["fields"][1]["kind"], "colour");
        assert!(listed["text"].as_str().unwrap().contains("1 saved: Guest"), "{listed}");
        assert!(title_templates(None, None)["text"].as_str().unwrap().contains("None saved"));

        let op = |name: &str| klipp_document::ops::Op::Template { name: name.into(), at: klipp_document::ops::Anchor::Secs(0.0), secs: 3.0, track: None, fields: Default::default(), data: None, composition: None };
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
        let listed = title_templates(None, None);
        let builtin = listed["builtin"].as_array().unwrap();
        assert_eq!(builtin.len(), klipp_timeline::title_library::TITLES.len());
        let bar = builtin.iter().find(|t| t["name"] == "Name Bar").unwrap();
        assert_eq!(bar["category"], "Lower Thirds");
        let labels: Vec<&str> = bar["fields"].as_array().unwrap().iter().map(|f| f["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Name", "Role", "Accent", "Size", "Speed"]);
        assert!(listed["text"].as_str().unwrap().contains("Lower Thirds: Name Bar"), "{}", listed["text"]);

        let op = |name: &str| klipp_document::ops::Op::Template { name: name.into(), at: klipp_document::ops::Anchor::Secs(0.0), secs: 3.0, track: None, fields: Default::default(), data: None, composition: None };
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


#[cfg(test)]
mod qc_geometry_tests {
    use super::AnalysedTranscripts;
    use klipp_document::ops::Transcripts;
    #[test]
    fn qc_crop_requests_inspection_for_portrait_fit_and_post_placement() {
        use klipp_core::{MediaId, Resolution, Timecode};
        use klipp_document::{ops::SpokenWord, qc, qc_geometry};
        use klipp_timeline::{Clip, ClipKind, Sequence, Shape, ShapeKind, Track, TrackKind, TitleData, TitleObjectContent, TitlePosition};
        struct Host;
        impl Transcripts for Host {
            fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> { None }
            fn faces(&self, _: &klipp_media::MediaItem) -> Vec<(f64,[f32;4])> { vec![(1.0,[0.1,0.1,0.1,0.1])] }
            fn picture_geometry(&self, seq: &Sequence, clip: &Clip, at: f64) -> Option<qc_geometry::Geometry> {
                AnalysedTranscripts.picture_geometry(seq,clip,at)
            }
            fn title_regions(&self, title: &TitleData, frame: Resolution, local: f64, duration: f64) -> Vec<[f32;4]> {
                AnalysedTranscripts.title_regions(title,frame,local,duration)
            }
        }
        let tc = |s| Timecode::from_secs_f64(s,1000);
        for portrait in [true,false] {
            let mut pool = klipp_media::MediaPool::new();
            let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/qc-crop.mov".into(),None);
            item.id = MediaId::new();
            item.resolution = if portrait { Resolution { width:1080,height:1920 } } else { Resolution { width:1920,height:1080 } };
            let id = pool.add(item);
            let mut clip = Clip::media(id,tc(0.0),tc(4.0),tc(0.0),tc(4.0));
            if !portrait {
                clip = klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::Transform {
                    position_x: 400.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5,
                    rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
                    rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
                }).build();
            }
            let mut video = Track::new("V",TrackKind::Video);
            video.clips.push(clip);
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width:1920,height:1080 };
            seq.tracks = vec![video];
            let face = qc_geometry::detections(&seq,&pool,&Host,0.0,4.0,true).samples[0].rect;
            // Both faces lie to the right of frame x=.25. Cropping the
            // fitted frame there must not silently erase their QC evidence.
            assert!(face[0]>0.25);
            let mut title = TitleData::single_text(Default::default());
            title.objects[0].content = TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius:0.0 }, fill:Some([255;4]), gradient:None, stroke:None, fit:None });
            title.objects[0].transform.center = TitlePosition { x:face[0]+face[2]/2.0,y:face[1]+face[3]/2.0 };
            title.objects[0].transform.size = TitlePosition { x:face[2],y:face[3] };
            let mut text = Track::new("T",TrackKind::Text);
            text.clips.push(Clip { kind:ClipKind::Title(title), ..Clip::gap(tc(0.0),tc(4.0)) });
            seq.tracks.insert(0,text);
            let before = qc::check(&seq,&pool,&Host);
            assert!(before.findings.iter().any(|f|f.rule=="title_over_face"), "{}",before.text());
            // The second case explicitly appends Crop AFTER Transform.
            let cropped = std::mem::replace(&mut seq.tracks[1].clips[0], Clip::gap(tc(0.0),tc(4.0)));
            seq.tracks[1].clips[0] = klipp_timeline::test_clip::TestClip::of(cropped).effect(klipp_effects::BuiltinEffect::Crop { left:0.25,right:0.0,top:0.0,bottom:0.0,feather:0.0 }).build();
            let after = qc::check(&seq,&pool,&Host);
            assert!(after.findings.iter().any(|f|f.rule=="title_overlap_unchecked"), "{}",after.text());
            assert!(!after.findings.iter().any(|f|f.rule=="title_over_face"));
        }
    }

    #[test]
    fn qc_title_regions_follow_rendered_animation_and_transparency() {
        use klipp_timeline::{Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition};
        let mut title = TitleData::single_text(Default::default());
        let object = &mut title.objects[0];
        object.content = TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([255;4]), gradient: None, stroke: None, fit: None });
        object.transform.center = TitlePosition { x: 0.2, y: 0.5 };
        object.transform.size = TitlePosition { x: 0.1, y: 0.1 };
        object.transform.center_x_keyframes = [0.2,0.8].into_iter().enumerate().map(|(i,value)|klipp_core::Keyframe {
            time_secs: i as f64, value, interpolation: klipp_core::Interpolation::Linear,
            tangent_in: (0.0,0.0), tangent_out: (0.0,0.0),
            handles: Default::default(),
        }).collect();
        let frame = klipp_core::Resolution { width: 1920, height: 1080 };
        let early = AnalysedTranscripts.title_regions(&title,frame,0.0,4.0);
        let late = AnalysedTranscripts.title_regions(&title,frame,1.0,4.0);
        assert!(!early.is_empty() && !late.is_empty());
        assert!(early.iter().all(|r| r[0]+r[2]<0.4));
        assert!(late.iter().all(|r| r[0]>0.6));
        title.objects[0].visible = false;
        assert!(AnalysedTranscripts.title_regions(&title,frame,1.0,4.0).is_empty());
        // The real adapter accepts an ordinary media picture, rather than
        // silently declaring every effect list unsupported.
        let mut seq = klipp_timeline::Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::Timecode::from_secs_f64(0.0,1000), klipp_core::Timecode::from_secs_f64(4.0,1000), klipp_core::Timecode::from_secs_f64(0.0,1000), klipp_core::Timecode::from_secs_f64(4.0,1000));
        let mut track = klipp_timeline::Track::new("V",klipp_timeline::TrackKind::Video);
        track.clips.push(clip);
        seq.tracks=vec![track];
        assert!(AnalysedTranscripts.picture_geometry(&seq,&seq.tracks[0].clips[0],1.0).is_some());
    }
}
