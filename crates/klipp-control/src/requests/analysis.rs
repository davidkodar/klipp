//! Analysis requests: analysing media, transcripts, screen text, redaction, sound effects, loudness and spectra, beat sheets, shots and excerpts (#826).

#[allow(unused_imports)]
use crate::*;

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
                let imported = handle(ctx, ControlCommand::ImportMedia { path: path.to_string_lossy().into_owned(), analyze: false }, &mut None)?;
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
                let reveals = |id: klipp_core::MediaId| pool.get(&id).and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).beatsheet).map(|b| b.reveals).unwrap_or_default();
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
            'items: for m in pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File) {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(shots) = report.shots.as_ref() else { continue };
                for (i, shot) in shots.shots.iter().enumerate() {
                    let (a, b) = (shot.start_secs, shot.end_secs);
                    if min_secs.is_some_and(|s| b - a < s) {
                        continue;
                    }
                    let fr = report.framing.as_ref().and_then(|f| f.shots.iter().find(|s| (s.start_secs - a).abs() < 0.05));
                    if let Some(want) = &framing {
                        if !fr.is_some_and(|f| f.framing == *want) {
                            continue;
                        }
                    }
                    if let Some(n) = min_people {
                        if !fr.is_some_and(|f| f.people >= n) {
                            continue;
                        }
                    }
                    if let Some(o) = &object {
                        if !fr.is_some_and(|f| f.objects.iter().any(|x| x.to_lowercase() == *o)) {
                            continue;
                        }
                    }
                    let motion_class = report.motion.as_ref().and_then(|mo| mo.class_within(a, b));
                    if let Some(want) = &motion {
                        if motion_class != Some(want.as_str()) {
                            continue;
                        }
                    }
                    let words = report.transcript.as_ref().map(|t| t.words_within(a, b).len()).unwrap_or(0);
                    if let Some(want) = with_speech {
                        if (words > 0) != want {
                            continue;
                        }
                    }
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

        ControlCommand::AudioExcerpt { media_id, from_secs, to_secs, path } => {
            if in_batch {
                return read_in_batch("audio_excerpt");
            }
            let (media_path, duration) = media_path_and_duration(ctx, &media_id)?;
            let out = temp_output(path, "klipp-excerpt", "wav")?;
            let from = from_secs.clamp(0.0, duration);
            let to = to_secs.clamp(from, duration);
            let p = klipp_analysis::sheet::audio_excerpt(std::path::Path::new("ffmpeg"), &media_path, from, to, &out)?;
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            Ok(json!({ "path": p.to_string_lossy(), "from_secs": from, "to_secs": to, "bytes": bytes, "format": "wav 16 kHz mono 16-bit" }))
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
