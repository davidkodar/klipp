//! Analysis requests: analysing media, transcripts, screen text, redaction, sound effects, loudness and spectra, beat sheets, shots and excerpts (#826).

#[allow(unused_imports)]
use crate::*;
use klipp_timeline::GradeAccess;

pub(crate) fn respond(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();

    match cmd {
        ControlCommand::Analyze { media_id, download_models, wait, timeout_ms } => {
            if in_batch {
                return read_in_batch("analyze");
            }
            let downloaded = if download_models { klipp_analysis::download_models()? } else { Vec::new() };
            // How the brief spells things goes to the transcriber; a
            // transcript made without those spellings is made again.
            let (spellings, language) = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.as_ref().map(|b| (b.spellings(), b.language.clone())).unwrap_or_default();
            klipp_analysis::transcript::set_vocabulary(spellings);
            // And the language it is spoken in, when the brief names one (#531).
            klipp_analysis::transcript::set_language(language);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let items: Vec<&klipp_media::MediaItem> = match media_id {
                Some(id) => {
                    let id = klipp_core::MediaId(id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
                    vec![pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?]
                }
                None => pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File && !m.offline).collect(),
            };
            let ids: Vec<_> = items.iter().map(|m| m.id).collect();
            let statuses: Vec<klipp_analysis::service::Status> = items.into_iter().map(klipp_analysis::service::request).collect();
            drop(pool);
            if wait {
                let mut reply = analysis_reply(&ids, true, timeout_ms)?;
                reply["models_downloaded"] = json!(downloaded);
                return Ok(reply);
            }
            let all_done = statuses.iter().all(|s| matches!(s.state, klipp_analysis::service::State::Done));
            let next = if all_done {
                "Already analysed. Read a transcript (with words) to choose the selects, and find_shots with no people for B-roll."
            } else {
                "Analysis runs in the background — shots, sound, motion, the transcript, framing. Watch analysis_status; a transcript of a long file is about a third of its length. Read each file's transcript and shots when it is done."
            };
            Ok(json!({ "items": statuses, "models_downloaded": downloaded, "next": next }))
        }

        ControlCommand::RestoreBackground { clip_id } => {
            let clip_id = existing_clip_id(ctx, &clip_id)?;
            {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let clip = seq.all_clips().find(|c| c.id == clip_id).ok_or("clip not found")?;
                if clip.grade().cut_out_matte().is_none() { return Err("This clip is not cut out by a subject or image matte".into()); }
            }
            let mut send = sender!(ctx, collect);
            send(AppCommand::RestoreBackground { clip_id })
        }

        ControlCommand::TrackSubject { clip_id, transition_id, model, download_model, into } => {
            if into.as_deref().map(str::trim) == Some("whole") {
                if transition_id.is_some() { return Err("into whole is for a clip, not a transition".into()); }
                let clip_id = clip_id.ok_or("give the clip_id whose background should be restored")?;
                return respond(ctx, ControlCommand::RestoreBackground { clip_id }, collect);
            }
            let cut_out = match into.as_deref().map(str::trim) {
                None | Some("grade") => false,
                Some("cut_out") => true,
                Some(other) => return Err(format!("into is \"grade\", \"cut_out\" or \"whole\", not \"{other}\"")),
            };
            if cut_out && transition_id.is_some() {
                return Err("a transition's subject is cut round by its pieces: into is for a clip".into());
            }
            use klipp_analysis::subject_matte::MatteModel;
            // A piece transition's subject (#985 phase 3): its side and
            // model are the transition's.
            let transition = match (&clip_id, &transition_id) {
                (Some(_), Some(_)) => return Err("give clip_id or transition_id, not both".into()),
                (None, None) => return Err("give the clip_id to track, or the transition_id of a piece transition cutting round its subject".into()),
                (None, Some(t)) => {
                    let want = klipp_core::TransitionId(parse_uuid(t, "transition_id")?);
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    let found = klipp_compose::subject_matte::transition_subjects(&seq).into_iter().find(|ts| ts.transition == want);
                    let on_track = seq.tracks.iter().any(|tr| tr.transitions.iter().any(|x| x.id == want));
                    match found {
                        Some(ts) => Some(ts),
                        None if on_track => return Err("that transition cuts round no subject: set its pieces' subject ({motion: stays, steps_forward or goes_first}) first".into()),
                        None => return Err(format!("no transition {t}")),
                    }
                }
                (Some(_), None) => None,
            };
            let name = match &transition {
                Some(ts) => (if ts.model == 1 { "foreground" } else { "subject" }).to_string(),
                None => model.as_deref().unwrap_or("subject").trim().to_ascii_lowercase(),
            };
            let which = match name.as_str() {
                "subject" => MatteModel::Subject,
                "foreground" => MatteModel::Foreground,
                other => return Err(format!("no model \"{other}\"; use subject or foreground")),
            };
            let clip = match (&transition, &clip_id) {
                (Some(ts), _) => ts.clip,
                (None, Some(c)) => existing_clip_id(ctx, c)?,
                (None, None) => unreachable!("refused above"),
            };
            let media = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let media = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip).and_then(|c| c.media_id()).and_then(|m| pool.get(&m)).ok_or("track_subject needs a clip with a picture from a file")?;
                if media.is_still || media.video_codec.is_empty() {
                    return Err("track_subject needs a video clip".into());
                }
                media.id
            };
            // Never fetched unasked: the model is named when it is missing.
            if download_model && in_batch {
                return Err("track_subject with download_model fetches the model first; send it on its own".into());
            }
            klipp_analysis::subject_matte::ensure_model(which, download_model).map_err(|e| format!("{e} (pass download_model: true to fetch it)"))?;
            let mut send = sender!(ctx, collect);
            let mut reply = match &transition {
                Some(ts) => send(AppCommand::TrackTransitionSubject { transition_id: ts.transition })?,
                None => send(AppCommand::TrackSubject { clip_id: clip, model: name, into: Some(if cut_out { klipp_core::SubjectMatteUse::CutOut } else { klipp_core::SubjectMatteUse::Grade }) })?,
            };
            reply["job_id"] = json!(format!("{}:{}", media.0, klipp_analysis::subject_matte::ID));
            reply["next"] = json!("The matte is made in the background and shows on the clip's grade as it goes. Read job_status with this job_id for its progress; cancel_job stops it and keeps the frames made.");
            Ok(reply)
        }

        ControlCommand::SearchTranscripts { query, max_hits } => {
            if in_batch {
                return read_in_batch("search_transcripts");
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let mut hits = Vec::new();
            let max = max_hits.unwrap_or(50);
            for m in pool.all() {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(t) = report.transcript.as_ref() else { continue };
                for seg in t.search(&query) {
                    hits.push(json!({
                        "media_id": m.id.0.to_string(),
                        "filename": m.filename,
                        "start_secs": seg.start_secs,
                        "end_secs": seg.end_secs,
                        "text": seg.text,
                    }));
                    if hits.len() >= max {
                        break;
                    }
                }
                if hits.len() >= max {
                    break;
                }
            }
            Ok(json!({ "query": query, "hits": hits }))
        }

        ControlCommand::ScreenText { media_id, from_secs, to_secs, all } => {
            if in_batch {
                return read_in_batch("screen_text");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(st) = report.screentext else {
                let installed = klipp_analysis::screentext::installed();
                let enabled = klipp_analysis::screen_text_enabled();
                return Ok(json!({
                    "text": format!("{}: no screen text read yet. {}", item.filename, if !enabled { "Screen text is switched off in preferences." } else if !installed { "The OCR models are not downloaded: analyze with download_models true (about 10 MB, the user should know), then read it again." } else { "Run analyze, then read it again." }),
                    "installed": installed,
                    "enabled": enabled,
                }));
            };
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(st.duration_secs));
            let within = st.spans_within(a, b);
            // The summary: what stayed up a second or more, or reads as a
            // heading — short, mostly letters, read with confidence — the
            // sidebar and the menu bar once each; not every line of body
            // text in every settings page.
            let heading_like = |s: &klipp_analysis::screentext::TextSpan| s.confidence >= 0.85 && s.text.split_whitespace().count() <= 6 && s.text.chars().filter(|c| c.is_alphabetic()).count() * 10 >= s.text.chars().count() * 7;
            let chosen: Vec<&klipp_analysis::screentext::TextSpan> = if all { within.clone() } else { within.iter().copied().filter(|s| s.end_secs - s.start_secs >= 1.0 || heading_like(s)).take(80).collect() };
            let spans: Vec<serde_json::Value> = chosen.iter().map(|s| json!({ "text": s.text, "start_secs": s.start_secs, "end_secs": s.end_secs, "x": s.x, "y": s.y, "w": s.w, "h": s.h, "confidence": s.confidence })).collect();
            let mut text = format!("{}  on-screen text {:.2}-{:.2} s ({} of {} lines{}, {} frames read)\n", item.filename, a, b, chosen.len(), within.len(), if all || chosen.len() == within.len() { "" } else { "; all true for every line" }, st.frames_read);
            for s in chosen.iter().take(if all { 400 } else { 80 }) {
                text.push_str(&format!("[{:.2}-{:.2}] ({:.2},{:.2}) {}\n", s.start_secs, s.end_secs, s.x, s.y, s.text));
            }
            for s in st.sensitive.iter().filter(|s| s.end_secs > a && s.start_secs < b) {
                text.push_str(&format!("SENSITIVE {} {:.2}-{:.2} s at ({:.2},{:.2}): {}\n", s.kind, s.start_secs, s.end_secs, s.x, s.y, s.masked));
            }
            Ok(json!({
                "text": text,
                "media_id": media_id,
                "spans": spans,
                "sensitive": st.sensitive,
                "frames_read": st.frames_read,
                "next": "Cut to where the text that matters is up and hold until it reads; a SENSITIVE line must be blurred or cut before delivery (check says so too).",
            }))
        }

        ControlCommand::SearchScreenText { query, max_hits } => {
            if in_batch {
                return read_in_batch("search_screen_text");
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let mut hits = Vec::new();
            let max = max_hits.unwrap_or(50);
            'files: for m in pool.all() {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(st) = report.screentext.as_ref() else { continue };
                for s in st.search(&query) {
                    hits.push(json!({ "media_id": m.id.0.to_string(), "filename": m.filename, "start_secs": s.start_secs, "end_secs": s.end_secs, "text": s.text, "x": s.x, "y": s.y, "w": s.w, "h": s.h }));
                    if hits.len() >= max {
                        break 'files;
                    }
                }
            }
            Ok(json!({ "query": query, "hits": hits }))
        }

        ControlCommand::SearchVisual { query, max_hits, min_score } => {
            if in_batch {
                return read_in_batch("search_visual");
            }
            let wanted = klipp_analysis::visual::query(&query)?;
            let min = min_score.unwrap_or(0.2);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let (mut hits, mut searched) = (Vec::new(), 0usize);
            for m in pool.all() {
                let Some(v) = klipp_analysis::visual::load(&m.path) else { continue };
                searched += 1;
                let length = m.duration.to_secs_f64();
                for h in v.search(&wanted, min) {
                    let end = if length > 0.0 { h.end_secs.min(length) } else { h.end_secs };
                    hits.push((h.score, json!({ "media_id": m.id.0.to_string(), "filename": m.filename, "start_secs": h.start_secs, "end_secs": end, "best_secs": h.best_secs, "score": (h.score * 1000.0).round() / 1000.0 })));
                }
            }
            hits.sort_by(|a, b| b.0.total_cmp(&a.0));
            hits.truncate(max_hits.unwrap_or(20));
            let mut out = json!({ "query": query, "searched": searched, "hits": hits.into_iter().map(|(_, h)| h).collect::<Vec<_>>() });
            if searched == 0 {
                out["next"] = json!("No footage has been looked at for picture search yet: download its model (klipp_analyze download_models) and analyze the media.");
            }
            Ok(out)
        }

        ControlCommand::AutoRedact { apply } => {
            if in_batch {
                return Err("auto_redact is its own batch; send it on its own".into());
            }
            use klipp_document::ops::{Anchor, ClipRef, Op, Transcripts as _};
            let ops: Vec<Op> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let mut ops = Vec::new();
                let mut cache: std::collections::HashMap<klipp_core::MediaId, Vec<klipp_document::ops::SensitiveText>> = std::collections::HashMap::new();
                for t in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
                    for c in t.clips.iter().filter(|c| c.enabled) {
                        let Some(media_id) = c.media_id() else { continue };
                        let Some(item) = pool.get(&media_id) else { continue };
                        let found = cache.entry(media_id).or_insert_with(|| AnalysedTranscripts.screen_sensitive(item));
                        let (s_in, s_out, t_in) = (c.source_in.to_secs_f64(), c.source_out.to_secs_f64(), c.timeline_in.to_secs_f64());
                        for s in found.iter().filter(|s| s.end_secs > s_in && s.start_secs < s_out) {
                            let from = t_in + (s.start_secs.max(s_in) - s_in);
                            let to = t_in + (s.end_secs.min(s_out) - s_in);
                            ops.push(Op::Redact { clip_id: ClipRef::Id(c.id), from: Some(Anchor::Secs(from)), to: Some(Anchor::Secs(to)), x: s.x, y: s.y, w: s.w, h: s.h, kind: "mosaic".into(), amount: None });
                        }
                    }
                }
                ops
            };
            if ops.is_empty() {
                return Ok(json!({ "text": "Nothing to redact: the screen text found nothing sensitive in the stretches the timeline shows (or it has not been read: analyze with the OCR models).", "steps_resolved": 0, "applied": false }));
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Auto Redact".into()))?;
                v["next"] = json!("Redacted. Look at a frame inside each stretch (frame_at) to see the box covers the text, then check again.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what auto_redact would hide. Run it with apply true to do it as one undo step." })
            };
            v["text"] = json!(plan.text());
            v["steps_resolved"] = json!(plan.steps.len());
            v["ops"] = json!(ops);
            Ok(v)
        }

        ControlCommand::AutoSfx { whoosh, click, riser, on, apply } => {
            if in_batch {
                return Err("auto_sfx is its own batch; send it on its own".into());
            }
            let on = on.as_deref().map(klipp_document::sound_design::SfxOn::parse).transpose()?.unwrap_or_default();
            let synthesized = whoosh.is_none() && click.is_none() && riser.is_none();
            if synthesized && !apply {
                let seq = ctx.sequence.read().unwrap_or_else(|e| e.into_inner());
                let placements = klipp_document::sound_design::preview_whoosh(&seq, f64::from(klipp_audio::WhooshVariant::Short.duration_secs()), on);
                let summary: Vec<_> = placements.iter().map(|p| format!("whoosh at {:.2} s", p.at_secs)).collect();
                return Ok(json!({ "text": format!("Would synthesize a short whoosh and place {} sounds: {}. Run auto_sfx with apply true to create the audio and place it.", summary.len(), summary.join("; ")), "placements": summary, "applied": false, "synthesized_whoosh": false }));
            }
            let whoosh = if synthesized {
                // Materialize and import the reusable asset only when applying.
                // Serializing this small path
                // avoids two socket clients writing/importing the same WAV.
                static SYNTH_IMPORT: Mutex<()> = Mutex::new(());
                let _guard = SYNTH_IMPORT.lock().unwrap_or_else(|e| e.into_inner());
                let dir = klipp_media::made::made_dir("auto-sfx");
                std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
                let path = dir.join("whoosh-short-v1.wav");
                if !path.is_file() {
                    let staging = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
                    let saved = klipp_audio::write_whoosh_wav(klipp_audio::WhooshVariant::Short, &staging)
                        .and_then(|_| std::fs::rename(&staging, &path));
                    if let Err(e) = saved {
                        let _ = std::fs::remove_file(&staging);
                        return Err(format!("synthesize whoosh: {e}"));
                    }
                }
                let imported = handle(ctx, ControlCommand::import_one(path.to_string_lossy().into_owned(), false), &mut None)?;
                let id = imported["media_id"].as_str().ok_or("the whoosh was synthesized but its import has not completed")?;
                Some(klipp_core::MediaId(parse_uuid(id, "media_id")?))
            } else { whoosh };
            let (placements, ops) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                for (name, id) in [("whoosh", whoosh), ("click", click), ("riser", riser)] {
                    if let Some(id) = id {
                        let item = pool.get(&id).ok_or_else(|| format!("{name}: no media item {}; media ids are in list_media", id.0))?;
                        if !item.has_audio {
                            return Err(format!("{name}: {} has no sound", item.filename));
                        }
                    }
                }
                let reveals = |id: klipp_core::MediaId| pool.get(&id).and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).screen_beatsheet(it).map(|b| b.reveals.clone())).unwrap_or_default();
                let loudest = |id: klipp_core::MediaId| if synthesized && Some(id) == whoosh { Some(0.15) } else { pool.get(&id).and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).audio).and_then(|a| a.loudest_secs) };
                let kit = klipp_document::sound_design::SfxKit { whoosh, click, riser, on };
                let mut placements = klipp_document::sound_design::placements(&seq, &pool, kit, &reveals, &loudest);
                // Each effect against what plays under it, not at one level
                // for files a dozen decibels apart (#792): a whoosh a little
                // over the bed, a click a little under, a riser level.
                for p in placements.iter_mut() {
                    let over = match p.what { "whoosh" => 3.0, "click" => -3.0, _ => 0.0 };
                    if let Some(v) = level_over_bed(&seq, &pool, p.media_id, p.at_secs, p.secs, over) {
                        p.volume_db = v;
                    }
                }
                let ops = klipp_document::sound_design::ops(&seq, &placements);
                (placements, ops)
            };
            if ops.is_empty() {
                return Ok(json!({ "text": match on {
                    klipp_document::sound_design::SfxOn::Cuts => "Nothing to place: no cut on the top picture track without an effect there already, and no reveal inside its clips (analyze gives the beat sheet).",
                    klipp_document::sound_design::SfxOn::Transitions => "Nothing to place: no cut on the top picture track carries a transition without an effect there already.",
                    klipp_document::sound_design::SfxOn::Stylised => "Nothing to place: no cut on the top picture track carries a stylised transition (zoom, whip, glitch, film burn…) without an effect there already.",
                }, "steps_resolved": 0, "applied": false }));
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let summary: Vec<String> = placements.iter().map(|p| format!("{} at {:.2} s", p.what, p.at_secs)).collect();
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Auto SFX".into()))?;
                v["next"] = json!("Placed. Play across a cut to hear the whoosh land on it; check reads the levels.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what auto_sfx would place. Run it with apply true to do it as one undo step." })
            };
            v["text"] = json!(format!("{}\n{}", summary.join("; "), plan.text()));
            v["placements"] = json!(summary);
            v["synthesized_whoosh"] = json!(synthesized);
            v["steps_resolved"] = json!(plan.steps.len());
            Ok(v)
        }

        ControlCommand::MixLoudness { from_secs, to_secs, step_secs } => {
            if in_batch {
                return read_in_batch("mix_loudness");
            }
            let snapshot = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear)
                    .map_err(|e| format!("the mix could not be read: {e}"))?
            };
            let end = snapshot.end_frame() as f64 / snapshot.fps.max(1.0);
            let from = from_secs.unwrap_or(0.0).clamp(0.0, end);
            let to = to_secs.unwrap_or(end).clamp(from, end);
            let step = step_secs.unwrap_or(0.5).clamp(0.1, 10.0);
            if to - from < step {
                return Err(format!("{from:.2}-{to:.2} s is shorter than one step of {step:.2} s"));
            }
            let rows = mix_loudness(&snapshot, from, to, step)?;
            let bed = {
                let mut st: Vec<f32> = rows.iter().map(|r| r.1).filter(|v| v.is_finite() && *v > -70.0).collect();
                st.sort_by(f32::total_cmp);
                st.get(st.len() / 2).copied()
            };
            let lines: Vec<String> = rows
                .iter()
                .map(|(t, short, peak)| {
                    let rise = bed.is_some_and(|b| *peak > b + 4.0);
                    format!("{t:7.2} s  short-term {short:6.1} LUFS  loudest 400 ms {peak:6.1}{}", if rise { "  ← stands out" } else { "" })
                })
                .collect();
            Ok(json!({
                "text": format!(
                    "The mix from {from:.1} to {to:.1} s, metered every {step:.2} s as the export mixes it, before loudness normalisation{}:\n{}",
                    bed.map(|b| format!(" (median short-term {b:.1} LUFS; a step whose loudest 400 ms is 4 dB over that stands out)")).unwrap_or_default(),
                    lines.join("\n"),
                ),
                "from_secs": from, "to_secs": to, "step_secs": step,
                "median_short_term_lufs": bed,
                "steps": rows.iter().map(|(t, s, p)| json!({ "secs": t, "short_term_lufs": s, "momentary_max_lufs": p })).collect::<Vec<_>>(),
                "next": "A whoosh or a hit should stand out by a few dB at its cut; an effect that does not is buried. audio sets a clip's level; mix_loudness again to hear the change.",
            }))
        }

        ControlCommand::AudioPlugins { query, param } => {
            // A broad query lists ids alone: a limiter suite has hundreds
            // of parameters a plug-in, which ran a list to 100 KB. The
            // parameters come when the query names one plug-in.
            const LISTED: usize = 30;
            let want = query.as_deref().map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
            let all = crate::plans::installed_audio_plugins();
            let exact = want.as_ref().and_then(|q| all.iter().find(|p| p.id.to_lowercase() == *q));
            let found: Vec<&klipp_document::ops::AudioPlugin> = match exact {
                Some(p) => vec![p],
                None => all.iter().filter(|p| want.as_ref().is_none_or(|q| p.name.to_lowercase().contains(q) || p.id.to_lowercase().contains(q))).collect(),
            };
            if let [p] = found[..] {
                let word = param.as_deref().map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty());
                let params: Vec<&klipp_document::ops::AudioPluginParam> = p.params.iter().filter(|c| word.as_ref().is_none_or(|w| c.name.to_lowercase().contains(w) || c.key.to_lowercase().contains(w))).collect();
                let mut text = format!("{} — {}, {} parameters", p.id, p.name, p.params.len());
                if let Some(w) = &word {
                    text.push_str(&format!(", {} holding \"{w}\"", params.len()));
                }
                text.push(':');
                for c in params.iter().take(LISTED) {
                    text.push_str(&format!("\n  {} ({}) {}..{}, default {}", c.name, c.key, c.min, c.max, c.default));
                }
                if params.len() > LISTED {
                    text.push_str(&format!("\n  and {} more: narrow with param", params.len() - LISTED));
                }
                let listed: Vec<serde_json::Value> = params.iter().take(LISTED).map(|c| json!({ "key": c.key, "name": c.name, "min": c.min, "max": c.max, "default": c.default })).collect();
                return Ok(json!({ "plugin": { "id": p.id, "name": p.name, "param_count": p.params.len(), "params": listed }, "text": text, "next": "Add it with the clip_audio or track_audio op: effect its id, params by name or key." }));
            }
            let mut text = match (all.len(), &want) {
                (0, _) => "No LV2 or CLAP audio plug-ins are installed here (the app looks in the usual LV2_PATH and CLAP_PATH folders when it starts).".to_string(),
                (n, Some(q)) => format!("{} of the {n} installed audio plug-ins match \"{q}\":", found.len()),
                (n, None) => format!("{n} installed audio plug-ins:"),
            };
            for p in found.iter().take(LISTED) {
                text.push_str(&format!("\n  {} — {} ({} parameters)", p.id, p.name, p.params.len()));
            }
            if found.len() > LISTED {
                text.push_str(&format!("\n  and {} more: narrow with query", found.len() - LISTED));
            }
            let listed: Vec<serde_json::Value> = found.iter().take(LISTED).map(|p| json!({ "id": p.id, "name": p.name, "param_count": p.params.len() })).collect();
            Ok(json!({ "plugins": listed, "installed": all.len(), "text": text, "next": "Query one plug-in's id (or a word only it holds) for its parameters." }))
        }

        ControlCommand::AudioSpectrum { media_id, from_secs, to_secs } => {
            if in_batch {
                return read_in_batch("audio_spectrum");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, dur, has_audio) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64(), item.has_audio)
            };
            if !has_audio {
                return Err(format!("{filename} has no sound in it"));
            }
            let from = from_secs.unwrap_or(0.0).max(0.0);
            let to = to_secs.filter(|t| *t > from);
            // Enough columns that a fault a second long is its own
            // column on anything up to an hour, and the cost is the
            // same whatever the file's length.
            let spec = klipp_analysis::spectrogram::of_file(std::path::Path::new("ffmpeg"), &path, from, to, 600, 128)?;
            // Music has no voice for the noise rules to measure against
            // (#723): its bass read as mains hum "under the voice". A
            // track from a library (its credit beside it), or media every
            // clip of plays as music, is not judged by them.
            let music = music_reason(ctx, &path, id);
            let found = if music.is_some() { Vec::new() } else { klipp_analysis::spectrogram::findings(&spec) };
            let lines: Vec<String> = found.iter().map(|(sev, what, fix)| format!("[{sev}] {what} — {fix}")).collect();
            // The bands as numbers, thinned so a reply stays readable:
            // the loudest column of each second.
            let per_second = (1.0 / spec.hop_secs.max(0.001)).round().max(1.0) as usize;
            let bands: Vec<serde_json::Value> = spec
                .bands
                .iter()
                .map(|b| {
                    let thinned: Vec<f32> = b.db.chunks(per_second).map(|c| c.iter().fold(-96.0f32, |a, v| a.max(*v))).collect();
                    json!({ "name": b.name, "low_hz": b.low_hz, "high_hz": b.high_hz, "db_per_second": thinned })
                })
                .collect();
            Ok(json!({
                "text": format!(
                    "The sound of {filename}{}, {:.1} s read in {:.2} s columns:\n{}{}",
                    if from > 0.0 || to.is_some() { format!(" from {:.1} s", from) } else { String::new() },
                    spec.duration_secs,
                    spec.hop_secs,
                    match &music {
                        Some(why) => format!("This is music ({why}): the noise rules measure hum, rumble, hiss and sibilance against a voice, and music has none to measure against; its low end is its bass. The bands below show its balance."),
                        None if lines.is_empty() => "Nothing to report: no band comes close enough to the voice to be heard under it.".to_string(),
                        None => lines.join("\n"),
                    },
                    if music.is_some() { "" } else { "\n\nEvery threshold here is a level *relative to the voice*, never an absolute one: a quiet recording of a clean room and a loud recording of a noisy one have the same absolute noise floor and different problems. Under about 30 dB below the voice a steady noise stops being audible under speech; at 20 dB down it is plainly there." },
                ),
                "media_id": media_id,
                "filename": filename,
                "duration_secs": spec.duration_secs,
                "media_duration_secs": dur,
                "hop_secs": spec.hop_secs,
                "rate": spec.rate,
                "findings": found.iter().map(|(sev, what, fix)| json!({ "severity": sev, "what": what, "fix": fix })).collect::<Vec<_>>(),
                "music": music.is_some(),
                "bands": bands,
                "next": if music.is_some() {
                    "For music, audio sets its level and duck lowers it under speech; the dialogue cleaners are for recorded sound."
                } else {
                    "clean_audio on the clip whose sound plays takes voice (Voice Isolation, for dialogue), wind, hiss, hum and deess; run audio_spectrum again to see whether it worked."
                },
            }))
        }

        ControlCommand::AnalysisQueue { media_id, cancel_pending } => {
            if in_batch { return read_in_batch("analysis_queue"); }
            let id = media_id.as_deref().map(|s| parse_uuid(s, "media_id").map(klipp_core::MediaId)).transpose()?;
            let cancelled = klipp_analysis::service::manage_queue(id, cancel_pending)?;
            Ok(json!({ "cancelled": cancelled, "items": klipp_analysis::service::all() }))
        }

        ControlCommand::AnalysisStatus { wait, media_id, timeout_ms } => {
            if in_batch {
                return read_in_batch("analysis_status");
            }
            let ids = match media_id {
                Some(id) => vec![klipp_core::MediaId(parse_uuid(&id, "media_id")?)],
                None => klipp_analysis::service::all().iter().map(|s| parse_uuid(&s.media_id, "media_id").map(klipp_core::MediaId)).collect::<Result<_, _>>()?,
            };
            let mut reply = analysis_reply(&ids, wait, timeout_ms)?;
            reply["models"] = json!({
                "speech": klipp_analysis::transcript::installed_model().map(|(id, _)| id),
                "detection": klipp_analysis::framing::model_path().map(|(id, _)| id),
            });
            // What each model opened runs on; a line on the CPU says why,
            // a GPU left unused among them (#2250).
            let mut lines: Vec<String> = reply["execution_providers"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
            for line in klipp_analysis::providers::lines() {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
            reply["execution_providers"] = json!(lines);
            reply["note"] = json!("current is the analyser at work and current_for_secs how long it has been; a transcript takes about a third of the file's length");
            Ok(reply)
        }

        ControlCommand::MediaAnalysis { media_id, max_chars } => {
            if in_batch {
                return read_in_batch("media_analysis");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let status = klipp_analysis::service::status(id).map(|s| s.state);
            // The whole of it, then cut: cut twice, the note gave the
            // length of the first cut as the length of the whole.
            let full = format!("{}  {:.2} s\n{}", item.filename, item.duration.to_secs_f64(), report.text(usize::MAX));
            let (text, cut) = cut_to_budget_with(&full, max_chars.unwrap_or(8000), "ask again with a larger max_chars, or read a stretch of the speech with transcript (from_secs, to_secs)");
            Ok(json!({
                "text": text,
                "cut": cut,
                "analysed": !report.is_empty(),
                "state": status,
                "shots": report.shots,
                "audio": report.audio,
                "transcript": report.transcript,
                "motion": report.motion,
                "framing": report.framing,
                "beatsheet": report.beatsheet,
            }))
        }

        ControlCommand::BeatSheet { media_id } => {
            if in_batch {
                return read_in_batch("beat_sheet");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let status = klipp_analysis::service::status(id);
            match report.beatsheet {
                Some(bs) => {
                    let mut text = format!("{}  {:.2} s\n{}", item.filename, item.duration.to_secs_f64(), bs.summary());
                    // With the screen read, every beat says what it showed.
                    let labels = report.screentext.as_ref().map(|st| klipp_analysis::screentext::label_beats(&bs, st));
                    if let Some(l) = &labels {
                        text.push_str(&klipp_analysis::screentext::labelled_beats_text(&bs, l));
                    }
                    let beats: Vec<serde_json::Value> = bs.segments.iter().enumerate().map(|(i, s)| json!({
                        "start_secs": s.start_secs, "end_secs": s.end_secs, "kind": s.kind,
                        "label": labels.as_ref().and_then(|l| l.segments.get(i).cloned().flatten()),
                    })).collect();
                    let reveals: Vec<serde_json::Value> = bs.reveals.iter().enumerate().map(|(i, r)| json!({
                        "secs": r, "label": labels.as_ref().and_then(|l| l.reveals.get(i).cloned().flatten()),
                    })).collect();
                    Ok(json!({
                        "text": text,
                        "state": status.as_ref().map(|s| &s.state),
                        "cut_points": bs.cut_points,
                        "title_windows": bs.title_windows,
                        "reveals": bs.reveals,
                        "spots": bs.spots,
                        "segments": bs.segments,
                        "beats": beats,
                        "reveals_labelled": reveals,
                        "labelled": labels.is_some(),
                        "next": if labels.is_some() { "Plan by name: each settled beat says what the screen showed. Cut to its reveal and hold until it reads; confirm with frame_at where it matters." } else { "Cut to a strong cut point and hold on a settled stretch until it reads; look at the frame there (frame_at/contact_sheet) to confirm what is shown and refine the exact cut. With the OCR models downloaded, analyze names every beat by its on-screen text." },
                    }))
                }
                None => {
                    use klipp_analysis::service::State;
                    let message = match status.as_ref() {
                        Some(st @ klipp_analysis::service::Status { state: State::Running, .. }) => {
                            format!("Analysis is running: {} ({} s on this pass; {} passes complete). Wait with analysis_status, this media_id and wait true, then read beat_sheet again.", st.current.unwrap_or("starting the next pass"), st.current_for_secs.unwrap_or(0), st.done.len())
                        }
                        Some(klipp_analysis::service::Status { state: State::Queued, .. }) => "Analysis is queued. Wait with analysis_status, this media_id and wait true; analysis_queue can prioritize it.".into(),
                        Some(klipp_analysis::service::Status { state: State::Failed, .. }) => "Analysis failed; see analysis.failed for the pass errors. Retry analyze after fixing the reported cause.".into(),
                        Some(klipp_analysis::service::Status { state: State::Cancelled, .. }) => "Analysis was cancelled. Call analyze with this media_id to request it again.".into(),
                        Some(klipp_analysis::service::Status { state: State::Done, .. }) => "Analysis finished without a beat sheet. Check that the shots and motion passes are enabled and support this media.".into(),
                        None => "Run analyze first (the beat sheet needs the shots and motion passes), then read it again.".into(),
                    };
                    Ok(json!({
                        "text": format!("{}: no beat sheet yet. {message}", item.filename),
                        "state": status.as_ref().map(|s| &s.state),
                        "analysis": status,
                    }))
                },
            }
        }

        ControlCommand::FindShots { framing, min_people, object, motion, with_speech, min_secs, max_hits } => {
            if in_batch {
                return read_in_batch("find_shots");
            }
            // A search over files still being analysed is a search over
            // part of the footage: said, with which files.
            let pending: Vec<String> = klipp_analysis::service::all()
                .into_iter()
                .filter(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running))
                .map(|s| s.filename)
                .collect();
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let max = max_hits.unwrap_or(50);
            let mut hits = Vec::new();
            let object = object.map(|o| o.to_lowercase());
            let framing = framing.map(|f| f.to_lowercase());
            let motion = motion.map(|m| m.to_lowercase());
            // Whether a stretch passes the search, from what is known of it.
            let passes = |len: f64, fr_framing: Option<&str>, people: Option<u32>, objects: &[String], motion_class: Option<&str>, words: usize| {
                !min_secs.is_some_and(|s| len < s)
                    && framing.as_ref().is_none_or(|want| fr_framing == Some(want.as_str()))
                    && min_people.is_none_or(|n| people.is_some_and(|p| p >= n))
                    && object.as_ref().is_none_or(|o| objects.iter().any(|x| x.to_lowercase() == *o))
                    && motion.as_ref().is_none_or(|want| motion_class == Some(want.as_str()))
                    && with_speech.is_none_or(|want| (words > 0) == want)
            };
            'items: for m in pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File) {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(shots) = report.shots.as_ref() else { continue };
                for (i, shot) in shots.shots.iter().enumerate() {
                    let (a, b) = (shot.start_secs, shot.end_secs);
                    let fr = report.framing.as_ref().and_then(|f| f.shots.iter().find(|s| (s.start_secs - a).abs() < 0.05));
                    let objects: Vec<String> = fr.map(|f| f.objects.clone()).unwrap_or_default();
                    let words_in = |x: f64, y: f64| report.transcript.as_ref().map(|t| t.words_within(x, y).len()).unwrap_or(0);
                    let motion_class = report.motion.as_ref().and_then(|mo| mo.class_within(a, b));
                    let words = words_in(a, b);
                    if passes(b - a, fr.map(|f| f.framing.as_str()), fr.map(|f| f.people), &objects, motion_class, words) {
                        hits.push(json!({
                            "media_id": m.id.0.to_string(),
                            "filename": m.filename,
                            "shot": i + 1,
                            "start_secs": a,
                            "end_secs": b,
                            "framing": fr.map(|f| f.framing.clone()),
                            "people": fr.map(|f| f.people),
                            "position": fr.map(|f| f.position.clone()),
                            "objects": fr.map(|f| f.objects.clone()),
                            "motion": motion_class,
                            "words": words,
                        }));
                        if hits.len() >= max {
                            break 'items;
                        }
                    }
                    // A long take's moments, each searched on its own
                    // movement and framing (#502); the shot's objects stand
                    // for its moments'.
                    let samples = report.framing.as_ref().map(|f| f.samples.as_slice()).unwrap_or(&[]);
                    for (k, mo) in klipp_analysis::moments::moments(a, b, report.motion.as_ref(), samples).iter().enumerate() {
                        let words = words_in(mo.start_secs, mo.end_secs);
                        if !passes(mo.end_secs - mo.start_secs, Some(mo.framing), Some(mo.people), &objects, mo.motion, words) {
                            continue;
                        }
                        hits.push(json!({
                            "media_id": m.id.0.to_string(),
                            "filename": m.filename,
                            "shot": i + 1,
                            "moment": k + 1,
                            "start_secs": mo.start_secs,
                            "end_secs": mo.end_secs,
                            "framing": mo.framing,
                            "people": mo.people,
                            "position": mo.position,
                            "objects": objects,
                            "motion": mo.motion,
                            "steady": mo.steady(),
                            "words": words,
                        }));
                        if hits.len() >= max {
                            break 'items;
                        }
                    }
                }
            }
            let mut v = json!({ "hits": hits });
            if !pending.is_empty() {
                v["still_analysing"] = json!(pending);
                v["note"] = json!(format!("these files are still being analysed and are not in the hits yet: {}; see analysis_status and ask again when they are done", pending.join(", ")));
            }
            Ok(v)
        }

        ControlCommand::ContactSheet { media_id, from_secs, to_secs, frames, cols, tile_width, path, inline } => {
            let inline = inline.unwrap_or(path.is_none());
            if in_batch {
                return read_in_batch("contact_sheet");
            }
            let (media_path, duration) = media_path_and_duration(ctx, &media_id)?;
            let out = temp_output(path, "klipp-sheet", "jpg")?;
            let from = from_secs.unwrap_or(0.0).clamp(0.0, duration);
            let to = to_secs.unwrap_or(duration).clamp(from, duration);
            let sheet = klipp_analysis::sheet::contact_sheet(
                std::path::Path::new("ffmpeg"), &media_path, from, to, frames.unwrap_or(12), cols.unwrap_or(4), tile_width.unwrap_or(240), &out,
            )?;
            let mut v = serde_json::to_value(sheet).map_err(|e| e.to_string())?;
            v["inline"] = json!(inline);
            Ok(v)
        }

        ControlCommand::AudioExcerpt { media_id, from_secs, to_secs, path, sequence } => {
            if in_batch {
                return read_in_batch("audio_excerpt");
            }
            let media_id = match (media_id, sequence) {
                (Some(_), true) => return Err("give media_id for a file's sound, or sequence for the timeline's mix, not both".into()),
                (Some(id), false) => id,
                // The timeline's range, heard as the export mixes it (#1577).
                (None, _) => {
                    let snapshot = {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                        klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear)
                            .map_err(|e| format!("the mix could not be read: {e}"))?
                    };
                    let end = snapshot.end_frame() as f64 / snapshot.fps.max(1.0);
                    let from = from_secs.clamp(0.0, end);
                    let to = to_secs.clamp(from, end).min(from + 60.0);
                    if to - from < 0.1 {
                        return Err(format!("{from:.2}-{to:.2} s holds no sound of the timeline (it ends at {end:.2} s)"));
                    }
                    let out = temp_output(path, "klipp-mix-excerpt", "wav")?;
                    let p = mix_excerpt(&snapshot, from, to, &out)?;
                    let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    return Ok(json!({ "path": p.to_string_lossy(), "from_secs": from, "to_secs": to, "bytes": bytes, "format": "wav 16 kHz mono 16-bit",
                        "source": "the sequence's mix, as the export mixes it, before loudness normalisation" }));
                }
            };
            let (media_path, duration) = media_path_and_duration(ctx, &media_id)?;
            let out = temp_output(path, "klipp-excerpt", "wav")?;
            let from = from_secs.clamp(0.0, duration);
            let to = to_secs.clamp(from, duration);
            let p = klipp_analysis::sheet::audio_excerpt(std::path::Path::new("ffmpeg"), &media_path, from, to, &out)?;
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            Ok(json!({ "path": p.to_string_lossy(), "from_secs": from, "to_secs": to, "bytes": bytes, "format": "wav 16 kHz mono 16-bit" }))
        }

        ControlCommand::Audition { action, media_id, clip_id, sequence, from_secs, to_secs, preview_urls } => {
            if in_batch {
                return read_in_batch("audition");
            }
            audition(ctx, action.as_deref(), media_id, clip_id, sequence, from_secs, to_secs, preview_urls)
        }

        ControlCommand::Transcript { media_id, from_secs, to_secs, words } => {
            if in_batch {
                return read_in_batch("transcript");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(t) = report.transcript else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript yet (analysis: {state}); analyze it, and see analysis_status", item.filename));
            };
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(f64::MAX));
            let silences: Vec<(f64, f64)> = report.audio.map(|au| au.silences.iter().map(|s| (s.start_secs, s.end_secs)).filter(|(x, y)| *y > a && *x < b).collect()).unwrap_or_default();
            let mut out = format!("{}  speech {} ({}), times in seconds of the file\n", item.filename, t.language.as_deref().unwrap_or("?"), t.model);
            for seg in t.segments.iter().filter(|s| s.end_secs > a && s.start_secs < b) {
                out.push_str(&format!("[{:.2}-{:.2}] {}\n", seg.start_secs, seg.end_secs, seg.text));
                if words {
                    let line: Vec<String> = t.words.iter().filter(|w| w.start_secs >= seg.start_secs - 0.01 && w.start_secs < seg.end_secs && w.end_secs > a && w.start_secs < b).map(|w| format!("{}@{:.2}-{:.2}", w.text, w.start_secs, w.end_secs)).collect();
                    out.push_str(&format!("    {}\n", line.join(" ")));
                }
            }
            if !silences.is_empty() {
                let line: Vec<String> = silences.iter().map(|(x, y)| format!("{x:.2}-{y:.2}")).collect();
                out.push_str(&format!("Measured silence (a cut there is clean): {}\n", line.join(", ")));
            }
            let (text, cut) = cut_to_budget_with(&out, 12_000, "ask for a shorter stretch with from_secs and to_secs");
            Ok(json!({ "text": text, "cut": cut }))
        }

        ControlCommand::AlignAudio { clip_id: c, reference_clip_id: r, max_offset_secs } => {
            if in_batch {
                return Err("align_audio moves a clip; send it on its own, not in a batch".into());
            }
            let tgt_id = clip_id(&c)?;
            let ref_id = clip_id(&r)?;
            // The two media paths and source-in points, and the reference's
            // place on the timeline — read, then let the lock go for ffmpeg.
            let (tgt_path, tgt_src_in, ref_path, ref_src_in, ref_tl_in, tgt_name) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let find = |id: klipp_core::ClipId| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(move |cl| cl.id == id).cloned();
                let tgt = find(tgt_id).ok_or_else(|| format!("no clip with id {c}"))?;
                let refc = find(ref_id).ok_or_else(|| format!("no clip with id {r}"))?;
                let media_of = |cl: &klipp_timeline::Clip, which: &str| -> Result<std::path::PathBuf, String> {
                    let mid = cl.media_id().ok_or_else(|| format!("the {which} clip has no media"))?;
                    let item = pool.get(&mid).ok_or_else(|| format!("the {which} clip's media is gone"))?;
                    if !item.has_audio {
                        return Err(format!("the {which} clip ({}) has no sound to sync", item.filename));
                    }
                    Ok(item.path.clone())
                };
                let tp = media_of(&tgt, "target")?;
                let rp = media_of(&refc, "reference")?;
                let tname = pool.get(&tgt.media_id().unwrap_or(klipp_core::MediaId(uuid::Uuid::nil()))).map(|m| m.filename.clone()).unwrap_or_else(|| "the clip".into());
                (tp, tgt.source_in.to_secs_f64(), rp, refc.source_in.to_secs_f64(), refc.timeline_in.to_secs_f64(), tname)
            };
            const SR: u32 = 8_000;
            let max_off = max_offset_secs.unwrap_or(30.0).clamp(1.0, 300.0);
            // Decode enough past the search window to hold a real match.
            let window = max_off + 90.0;
            let ffmpeg = std::path::Path::new("ffmpeg");
            let ref_pcm = klipp_analysis::audio_sync::decode_mono_pcm(ffmpeg, &ref_path, SR, ref_src_in, window)?;
            let tgt_pcm = klipp_analysis::audio_sync::decode_mono_pcm(ffmpeg, &tgt_path, SR, tgt_src_in, window)?;
            let (off_samples, confidence) = klipp_analysis::audio_sync::find_sync_offset_scored(&ref_pcm, &tgt_pcm, SR, max_off as f32);
            let offset_secs = off_samples as f64 / SR as f64;
            if confidence < 0.2 {
                return Err(format!("{tgt_name} and the reference do not seem to share any sound (match {:.0}%); nothing moved. Use a reference clip recorded at the same moment.", confidence * 100.0));
            }
            let new_in = ref_tl_in - offset_secs;
            if new_in < 0.0 {
                return Err(format!("the offset {offset_secs:+.3} s would push {tgt_name} before the timeline start; move the reference clip later first, then align"));
            }
            use klipp_document::ops::{Anchor, ClipRef, Op};
            let op = Op::Move { clip_id: ClipRef::Id(tgt_id), to: Anchor::Secs(new_in), track: None, linked: true };
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(format!("sync {tgt_name}")))?;
            v["text"] = json!(format!("Aligned {tgt_name} to the reference: offset {offset_secs:+.3} s, match {:.0}%. Moved it to {new_in:.3} s on the timeline.", confidence * 100.0));
            v["offset_secs"] = json!(offset_secs);
            v["confidence"] = json!(confidence);
            v["next"] = json!("Play across the join to confirm the sound and picture line up. If it drifts over a long take, the two clocks differ — you may need a second sync point.");
            Ok(v)
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// The most an audition plays of one stretch, and how many previews it
/// queues: as `audio_excerpt`'s cut, and `listen`'s five.
const AUDITION_SECS: f64 = 60.0;
const AUDITION_PREVIEWS: usize = 5;

/// What an audition plays, checked before anything is cut or fetched.
#[derive(Debug, PartialEq)]
enum AuditionSource {
    Media { id: String, from: f64, to: f64 },
    Clip { id: String, from: Option<f64>, to: Option<f64> },
    Mix { from: f64, to: f64 },
    Previews(Vec<String>),
}

fn audition_source(media_id: Option<String>, clip_id: Option<String>, sequence: bool, from: Option<f64>, to: Option<f64>, urls: Vec<String>) -> Result<AuditionSource, String> {
    let given = [media_id.is_some(), clip_id.is_some(), sequence, !urls.is_empty()].iter().filter(|g| **g).count();
    if given != 1 {
        return Err("audition plays one thing: media_id, clip_id, sequence true, or preview_urls".into());
    }
    let range = |what: &str| -> Result<(f64, f64), String> {
        let (Some(a), Some(b)) = (from, to) else { return Err(format!("{what} needs from_secs and to_secs")) };
        if !(a.is_finite() && b.is_finite() && a >= 0.0 && b > a) {
            return Err("from_secs and to_secs are a stretch: 0 or more, to after from".into());
        }
        Ok((a, b.min(a + AUDITION_SECS)))
    };
    Ok(if let Some(id) = media_id {
        let (from, to) = range("a media item")?;
        AuditionSource::Media { id, from, to }
    } else if let Some(id) = clip_id {
        AuditionSource::Clip { id, from, to }
    } else if sequence {
        let (from, to) = range("the mix")?;
        AuditionSource::Mix { from, to }
    } else {
        if urls.len() > AUDITION_PREVIEWS {
            return Err(format!("at most {AUDITION_PREVIEWS} previews at once, played in turn"));
        }
        if let Some(bad) = urls.iter().find(|u| !u.starts_with("https://")) {
            return Err(format!("{bad:?} is not a preview URL from a search result"));
        }
        AuditionSource::Previews(urls)
    })
}

/// `path`'s sound from `from` to `to` seconds of the file, as a WAV at the
/// file's own sample rate and channels (#1891): what an editor hears,
/// not `audio_excerpt`'s 16 kHz mono reading copy.
fn source_rate_wav(path: &std::path::Path, from: f64, to: f64) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-ss", &format!("{from:.3}"), "-t", &format!("{:.3}", (to - from).max(0.0))])
        .arg("-i")
        .arg(path)
        .args(["-vn", "-c:a", "pcm_s16le", "-f", "wav", "pipe:1"])
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.len() <= 44 {
        return Err(format!("no sound could be cut from {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// `audition` (#1891, #1927): play, stop, or say what is playing.
#[allow(clippy::too_many_arguments)]
fn audition(ctx: &ControlContext, action: Option<&str>, media_id: Option<String>, clip_id: Option<String>, sequence: bool, from: Option<f64>, to: Option<f64>, urls: Vec<String>) -> Result<serde_json::Value, String> {
    use klipp_audio_search::audition as player;
    let said = |p: &player::Playing| if p.active { format!("Playing for the person at the machine: {}.", p.queue.join(", then ")) } else { "Nothing is playing.".to_string() };
    match action.unwrap_or("play") {
        "stop" => {
            player::stop();
            return Ok(json!({ "playing": false, "text": "Stopped." }));
        }
        "status" => {
            let p = player::playing();
            return Ok(json!({ "playing": p.active, "queue": p.queue, "text": said(&p) }));
        }
        "play" => {}
        other => return Err(format!("audition action is play, stop or status, not {other:?}")),
    }
    let source = audition_source(media_id, clip_id, sequence, from, to, urls)?;
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut pieces: Vec<player::Piece> = Vec::new();
    let mut add = |label: String, bytes: Vec<u8>, from: Option<f64>, to: Option<f64>| {
        let (rate, channels) = player::wav_format(&bytes).map_or((None, None), |(r, c)| (Some(r), Some(c)));
        items.push(json!({ "source": label, "from_secs": from, "to_secs": to, "secs": from.zip(to).map(|(a, b)| b - a), "sample_rate": rate, "channels": channels }));
        pieces.push(player::Piece { label, bytes });
    };
    match source {
        AuditionSource::Media { id, from, to } => {
            let (path, duration) = media_path_and_duration(ctx, &id)?;
            let to = to.min(duration);
            let name = path.file_name().map_or(id.clone(), |n| n.to_string_lossy().into_owned());
            add(format!("{name} {from:.2}-{to:.2} s"), source_rate_wav(&path, from, to)?, Some(from), Some(to));
        }
        AuditionSource::Clip { id, from, to } => {
            let cid = crate::lookups::clip_id(&id)?;
            let (path, name, src_from, src_to) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let clip = seq.all_clips().find(|c| c.id == cid).ok_or_else(|| format!("no clip {id}"))?;
                let item = clip.media_id().and_then(|m| pool.get(&m)).filter(|m| m.has_audio).ok_or_else(|| format!("clip {id} has no sound to play"))?;
                let (t_in, t_out) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                let (a, b) = (from.unwrap_or(t_in).clamp(t_in, t_out), to.unwrap_or(t_out).clamp(t_in, t_out));
                let s_in = clip.source_in.to_secs_f64();
                (item.path.clone(), item.filename.clone(), s_in + (a - t_in), s_in + (b - t_in).min(a - t_in + AUDITION_SECS))
            };
            add(format!("clip {name}, source {src_from:.2}-{src_to:.2} s"), source_rate_wav(&path, src_from, src_to)?, Some(src_from), Some(src_to));
        }
        AuditionSource::Mix { from, to } => {
            let snapshot = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear)
                    .map_err(|e| format!("the mix could not be read: {e}"))?
            };
            let end = snapshot.end_frame() as f64 / snapshot.fps.max(1.0);
            let (from, to) = (from.min(end), to.min(end));
            if to - from < 0.1 {
                return Err(format!("{from:.2}-{to:.2} s holds no sound of the timeline (it ends at {end:.2} s)"));
            }
            let out = temp_output(None, "klipp-audition-mix", "wav")?;
            let p = mix_excerpt_as(&snapshot, from, to, &out, None, 2)?;
            let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let _ = std::fs::remove_file(&p);
            add(format!("the mix {from:.2}-{to:.2} s"), bytes, Some(from), Some(to));
        }
        AuditionSource::Previews(urls) => {
            for (i, url) in urls.iter().enumerate() {
                let bytes = klipp_audio_search::download_bytes(url)?;
                add(format!("preview {}", i + 1), bytes, None, None);
            }
        }
    }
    player::play(pieces)?;
    let p = player::playing();
    Ok(json!({
        "playing": items, "heard_by": "the person at the machine",
        "text": format!("{} The assistant does not hear it: ask the person what they hear.", said(&p)),
        "next": "audition with action stop ends it; status says what is playing.",
    }))
}

#[cfg(test)]
mod audition_tests {
    use super::*;

    /// #1891: one source at a time, a stretch for a file or the mix, at
    /// most a minute of it, at most five previews, and only search URLs.
    #[test]
    fn an_audition_plays_one_thing_and_says_what_it_needs() {
        let id = || Some("m".to_string());
        assert!(audition_source(None, None, false, None, None, vec![]).unwrap_err().contains("one thing"));
        assert!(audition_source(id(), None, true, Some(0.0), Some(1.0), vec![]).is_err(), "two at once");
        assert!(audition_source(id(), None, false, None, None, vec![]).unwrap_err().contains("from_secs"));
        assert_eq!(audition_source(id(), None, false, Some(10.0), Some(500.0), vec![]), Ok(AuditionSource::Media { id: "m".into(), from: 10.0, to: 70.0 }), "a minute at most");
        assert!(audition_source(None, None, true, Some(5.0), Some(2.0), vec![]).is_err(), "to before from");
        assert_eq!(audition_source(None, Some("c".into()), false, None, None, vec![]), Ok(AuditionSource::Clip { id: "c".into(), from: None, to: None }));
        let six: Vec<String> = (0..6).map(|i| format!("https://cdn.freesound.org/{i}.mp3")).collect();
        assert!(audition_source(None, None, false, None, None, six).unwrap_err().contains("at most 5"));
        assert!(audition_source(None, None, false, None, None, vec!["file:///etc/passwd".into()]).is_err());
    }

    /// #1891: the stretch played is cut at the file's own rate and
    /// channels; the mix's reading copy is 16 kHz mono. Skipped, said,
    /// without ffmpeg.
    #[test]
    fn an_audition_keeps_the_sources_rate_and_channels() {
        let dir = std::env::temp_dir().join(format!("klipp-audition-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("stereo.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=f=440:d=3:r=44100", "-ac", "2"])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("UNVERIFIED: no ffmpeg, the cut is not checked");
            return;
        }
        let bytes = source_rate_wav(&src, 0.5, 1.5).unwrap();
        assert_eq!(klipp_audio_search::audition::wav_format(&bytes), Some((44_100, 2)));
        // About a second of 16-bit stereo at 44.1 kHz.
        assert!((bytes.len() as f64 - 44.0 - 176_400.0).abs() < 4_000.0, "{} bytes", bytes.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
