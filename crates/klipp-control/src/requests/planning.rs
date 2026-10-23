//! Planning requests: orientation, the brief, the edit plan, checks, and batches (#826).

#[allow(unused_imports)]
use crate::*;

pub(crate) fn respond(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();
    let mut send = sender!(ctx, collect);

    match cmd {
        ControlCommand::Orient => {
            if in_batch {
                return read_in_batch("orient");
            }
            let project = project_status(ctx)?;
            let (brief, edit_plan) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                (seq.brief.clone(), seq.edit_plan.clone())
            };
            let statuses = klipp_analysis::service::all();
            let state_of = |path: &std::path::Path| -> serde_json::Value {
                let filename = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                match statuses.iter().find(|s| s.filename == filename) {
                    Some(s) => json!({ "state": format!("{:?}", s.state).to_lowercase(), "done": s.done, "current": s.current, "current_for_secs": s.current_for_secs }),
                    None => json!({ "state": "not analysed" }),
                }
            };
            let (media, all_analysed, any_running) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let media: Vec<serde_json::Value> = pool.all().map(|m| json!({
                    "media_id": m.id.0.to_string(),
                    "filename": m.filename,
                    "duration_secs": m.duration.to_secs_f64(),
                    "width": m.resolution.width,
                    "height": m.resolution.height,
                    "fps": m.fps.as_f64(),
                    "has_audio": m.has_audio,
                    "analysis": state_of(&m.path),
                })).collect();
                let all_analysed = !media.is_empty() && statuses.iter().filter(|s| pool.all().any(|m| m.filename == s.filename)).all(|s| matches!(s.state, klipp_analysis::service::State::Done));
                let any_running = statuses.iter().any(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running));
                (media, all_analysed, any_running)
            };
            let (seq_text, clips) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
                (timeline_text(&seq.with_own_grades(), &pool, None, ctx.shared.revision(), TimelineView::default()), clips)
            };
            // What to do next, from where the work stands.
            let next = if media.is_empty() {
                "Import the footage with import_media. Then set the brief with set_brief (its title, length, platform, loudness, captions, and the cast with names and spellings), and analyze."
            } else if brief.is_none() {
                "Set the brief with set_brief: the piece's title, target length, platform, loudness, whether it is captioned, and the cast with their names and how things are spelt. The spellings sharpen the transcript."
            } else if any_running {
                "Analysis is still running (see the media states). Read the transcript and the shots for the files that are done, and pull selects, while the rest finishes."
            } else if !all_analysed {
                "Analyze the footage: analyze runs shots, sound, motion, the transcript and framing. Then read a transcript to pull selects, and find_shots for B-roll."
            } else if clips == 0 && edit_plan.is_none() {
                "The footage is analysed. Propose the paper edit: read each file's beat_sheet (and transcript, where there is speech) and set_edit_plan with one segment per beat of the brief's arc — a stretch of a file for each ranked feature, a note saying what it shows, titles where the beat sheet has a hold window. Then show it to the user for approval."
            } else if clips == 0 && edit_plan.as_ref().is_some_and(|p| !p.approved) {
                "An edit plan is proposed and awaits approval. Show it to the user (edit_plan gives the text); change it with set_edit_plan if they ask; on their word, approve_edit_plan, then build_edit_plan."
            } else if clips == 0 {
                "The edit plan is approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step."
            } else {
                "Clips are down. Look with frame_at and contact_sheet, run check to hold the cut to the brief and the craft, fix what it finds, then export at the brief's loudness and save_project."
            };
            // Offline files first: nothing else reads right until they are
            // found (#2480).
            let offline = super::media::offline_media(ctx);
            let next = match &offline {
                Some(o) => format!("{} Then: {next}", o["text"].as_str().unwrap_or("")),
                None => next.to_string(),
            };
            Ok(json!({
                "build": build_json(),
                "offline_media": offline,
                "project": {
                    "path": project.path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "unsaved_changes": project.dirty,
                    "unsaved_changes_from": if project.dirty { project.changed_by.clone().or(Some("edits".into())) } else { None },
                },
                "brief": brief.as_ref().map(|b| b.text()),
                "edit_plan": edit_plan.as_ref().map(|p| p.text(brief.as_ref().and_then(|b| b.target_secs))),
                "media": media,
                "sequence": seq_text,
                "clips": clips,
                "revision": ctx.shared.revision(),
                "next": next,
                "guide": "Call klipp_guide for how to work here, the craft rules and worked recipes.",
            }))
        }

        ControlCommand::Brief => {
            if in_batch {
                return read_in_batch("brief");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            Ok(match &seq.brief {
                Some(b) => json!({ "brief": b, "text": b.text() }),
                None => json!({ "brief": null, "text": "There is no brief. Set one with set_brief: what the piece is about, its length, platform, loudness, whether it is captioned, and the cast with names and roles as they should appear." }),
            })
        }

        ControlCommand::EditPlan => {
            if in_batch {
                return read_in_batch("edit_plan");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            Ok(match &seq.edit_plan {
                Some(p) => {
                    let ops = klipp_document::edit_plan::to_ops(p, &seq, &pool, seq.brief.as_ref());
                    json!({
                        "plan": p,
                        "text": p.text(seq.brief.as_ref().and_then(|b| b.target_secs)),
                        "approved": p.approved,
                        "ops": ops,
                        "next": if p.approved { "Approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step." } else { "Show the plan to the user and ask for their approval; on their word, approve_edit_plan, then build_edit_plan." },
                    })
                }
                None => json!({ "plan": null, "text": "There is no edit plan. Propose one with set_edit_plan: the segments in order, each a stretch of a file for a feature of the brief, with a note saying what it shows and why; titles, joins and a music bed as the piece wants them." }),
            })
        }

        ControlCommand::SetEditPlan { mut plan } => {
            if in_batch {
                return Err("set_edit_plan is its own undo step; send it on its own".into());
            }
            let (old, brief, text, snapped) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_document::edit_plan::validate(&plan, &pool, seq.brief.as_ref())?;
                // Cut on the bars: the bed's beats, read now, round every
                // segment to whole bars and start the bed on its first beat.
                let mut snapped: Vec<String> = Vec::new();
                if plan.snap_to_bars {
                    let Some(m) = &plan.music else { return Err("snap_to_bars needs music in the plan: the bars are the bed's".into()) };
                    let path = pool.get(&m.media_id).map(|it| it.path.clone()).ok_or("music: no such media item")?;
                    let beats = klipp_analysis::beats::detect(std::path::Path::new("ffmpeg"), &path, 0.0)?;
                    if beats.bpm <= 0.0 || beats.beat_times.is_empty() {
                        snapped.push("no beat was found in the music; nothing snapped".into());
                    } else {
                        let bar = 240.0 / beats.bpm as f64;
                        snapped = klipp_document::edit_plan::snap_to_bars(&mut plan, bar, beats.beat_times[0], &pool);
                        snapped.insert(0, format!("bars of {bar:.3} s at {:.1} bpm", beats.bpm));
                        klipp_document::edit_plan::validate(&plan, &pool, seq.brief.as_ref())?;
                    }
                }
                // A proposal is never its own approval.
                plan.approved = false;
                let text = plan.text(seq.brief.as_ref().and_then(|b| b.target_secs));
                (seq.edit_plan.clone(), seq.brief.clone(), text, snapped)
            };
            let missing: Vec<String> = brief.as_ref().map(|b| b.ranked_features().iter().filter(|f| !plan.segments.iter().any(|s| klipp_document::edit_plan::covers_feature(s, &f.title))).map(|f| f.title.clone()).collect()).unwrap_or_default();
            let edit = klipp_document::history::EditCommand::SetEditPlan { old: old.map(Box::new), new: Some(Box::new(plan)) };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Set Edit Plan".into()))?;
            v["text"] = json!(text);
            if !missing.is_empty() {
                v["note"] = json!(format!("the brief's features not in the plan: {}", missing.join(", ")));
            }
            if !snapped.is_empty() {
                v["snapped"] = json!(snapped);
            }
            v["next"] = json!("Show the plan to the user, as text, and ask whether to build it. On their word call approve_edit_plan; then build_edit_plan to see what it builds, and build_edit_plan with apply true to build it.");
            Ok(v)
        }

        ControlCommand::ApproveEditPlan => {
            if in_batch {
                return Err("approve_edit_plan is its own undo step; send it on its own".into());
            }
            let old = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.edit_plan.clone();
            let Some(old) = old else {
                return Err("there is no edit plan to approve; propose one with set_edit_plan".into());
            };
            if old.approved {
                return Ok(json!({ "approved": true, "revision": ctx.shared.revision(), "text": "The edit plan is already approved.", "next": "build_edit_plan shows what it builds; with apply true it builds it." }));
            }
            let mut new = old.clone();
            new.approved = true;
            let edit = klipp_document::history::EditCommand::SetEditPlan { old: Some(Box::new(old)), new: Some(Box::new(new)) };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Approve Edit Plan".into()))?;
            v["approved"] = json!(true);
            v["next"] = json!("Approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step. Then look (frame_at, contact_sheet), check, and export.");
            Ok(v)
        }

        ControlCommand::BuildEditPlan { apply, detail } => {
            if in_batch {
                return Err("build_edit_plan is its own batch; send it on its own".into());
            }
            let ops = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let Some(plan) = seq.edit_plan.as_ref() else {
                    return Err("there is no edit plan to build; propose one with set_edit_plan".into());
                };
                if !plan.approved {
                    return Err("the edit plan is not approved; show it to the user and, on their word, approve_edit_plan".into());
                }
                klipp_document::edit_plan::validate(plan, &pool, seq.brief.as_ref()).map_err(|e| format!("the edit plan no longer builds: {e}"))?;
                klipp_document::edit_plan::to_ops(plan, &seq, &pool, seq.brief.as_ref())
            };
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Build Edit Plan".into()))?;
                v["next"] = json!("Built. Look at it (frame_at at each cut, contact_sheet), run check to hold it to the brief and the craft, fix what it finds, then export at the brief's loudness and save_project.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what it builds. build_edit_plan with apply true builds it as one undo step." })
            };
            v["text"] = json!(if detail { plan.text() } else { plan.text_folded() });
            with_warnings(&mut v, op_font_warnings(&ops));
            v["steps_resolved"] = json!(plan.steps.len());
            v["duration_secs"] = json!(plan.duration_secs);
            v["ops"] = json!(ops);
            if detail {
                v["steps"] = json!(plan.report());
            }
            Ok(v)
        }

        ControlCommand::UpdateBrief { fields } => {
            if in_batch {
                return Err("update_brief is its own undo step; send it on its own".into());
            }
            let current = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone().unwrap_or_default();
            let brief = merge_brief(&current, &fields)?;
            handle(ctx, ControlCommand::SetBrief { brief }, &mut None)
        }

        ControlCommand::SetBrief { mut brief } => {
            if in_batch {
                return Err("set_brief is its own undo step; send it on its own".into());
            }
            for style in &brief.type_styles {
                klipp_document::title_style::resolve_motion(style)
                    .map_err(|e| format!("type style {:?}: {e}", style.name))?;
                if let Some(shadow) = &style.shadow {
                    klipp_document::ops::type_style_shadow(shadow).map_err(|e| format!("type style {:?}: {e}", style.name))?;
                }
            }
            if let Some(t) = brief.target_secs {
                if !t.is_finite() || t <= 0.0 {
                    return Err(format!("a length of {t} s is not a length"));
                }
            }
            if let Some(l) = brief.loudness_lufs {
                if !(-40.0..=-5.0).contains(&l) {
                    return Err(format!("{l} LUFS is not a delivery loudness; the web is about -14, broadcast -23"));
                }
            }
            if brief.width.is_some() != brief.height.is_some() {
                return Err("give both width and height, or neither".into());
            }
            if let Some(dup) = brief.cast.iter().enumerate().find(|(i, c)| brief.cast[..*i].iter().any(|d| d.name.trim().eq_ignore_ascii_case(c.name.trim()))) {
                return Err(format!("{} is in the cast twice", dup.1.name));
            }
            if brief.cast.iter().any(|c| c.name.trim().is_empty()) {
                return Err("a cast member needs a name".into());
            }
            if brief.features.iter().any(|f| f.title.trim().is_empty()) {
                return Err("a feature needs a title".into());
            }
            if let Some(dup) = brief.features.iter().enumerate().find(|(i, f)| brief.features[..*i].iter().any(|d| d.title.trim().eq_ignore_ascii_case(f.title.trim()))) {
                return Err(format!("the feature \"{}\" is listed twice", dup.1.title));
            }
            if let Some(look) = brief.look.take() {
                let name = look.name.clone();
                let resolved = resolve_look_target(look);
                if resolved.mid_l.is_none() && resolved.cast_a.is_none() && !resolved.screencast {
                    return Err(format!("look \"{name}\" is neither a preset ({}) nor numbers; give a name, or the fields black_l, white_l, mid_l, contrast_range, chroma_mean, cast_a, cast_b, skin_hue, protect — style_profile lifts them from a reference", klipp_color::measure::LookTarget::PRESETS.join(", ")));
                }
                brief.look = Some(resolved);
            }
            if let Some(c) = brief.brand_colors.iter().find(|c| { let c = c.trim(); !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())) }) {
                return Err(format!("\"{c}\" is not a brand colour; give #rrggbb"));
            }
            let old = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone();
            let fonts_named: Vec<String> = brief.type_styles.iter().filter_map(|t| t.font.clone()).collect();
            let text = brief.text();
            let new_spellings = brief.spellings();
            // The titles that follow a style the brief changes go with it,
            // in the same step (#66).
            let followers = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                klipp_document::ops::follow_brief(&seq, old.as_ref(), Some(&brief))
            };
            let restyled = followers.len();
            let edit = klipp_document::history::EditCommand::SetBrief { old: old.clone().map(Box::new), new: Some(Box::new(brief)) };
            let respelt = old.as_ref().map(|b| b.spellings()).unwrap_or_default() != new_spellings;
            let edits: Vec<_> = std::iter::once(edit).chain(followers).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, None, Some("Set Brief".into()))?;
            if restyled > 0 {
                v["restyled_titles"] = json!(restyled);
            }
            v["text"] = json!(text);
            with_warnings(&mut v, font_warnings(fonts_named.iter().map(String::as_str)));
            if respelt && !new_spellings.is_empty() {
                // Only when a transcript was made without them: one that
                // already carries them is as good as it will get.
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let lacking: Vec<String> = pool
                    .all()
                    // Only what this project has analysed: a transcript in
                    // the cache from another day is re-made when the item
                    // is analysed here, and is nothing to warn about yet.
                    .filter(|m| klipp_analysis::service::status(m.id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Done)))
                    .filter_map(|m| klipp_analysis::report::MediaAnalysis::load(&m.path).transcript.map(|t| (m.filename.clone(), t.vocabulary)))
                    .filter(|(_, had)| klipp_analysis::transcript::lacks_spellings(had, &new_spellings))
                    .map(|(f, _)| f)
                    .collect();
                let analysed_any = pool.all().any(|m| klipp_analysis::service::status(m.id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Done)));
                if !lacking.is_empty() {
                    v["note"] = json!(format!("the spellings changed and these transcripts were made without them: {}; run analyze to have them transcribed again before captions or a transcript search", lacking.join(", ")));
                } else if analysed_any {
                    v["note"] = json!("the transcripts made so far already carry these spellings; a file analysed from now on gets them too");
                }
            }
            Ok(v)
        }

        ControlCommand::Check { color, loudness, min_severity } => {
            if in_batch {
                return read_in_batch("check");
            }
            let floor = severity_rank(min_severity.as_deref().unwrap_or("note")).ok_or_else(|| format!("min_severity {:?}: give error, warning or note", min_severity.as_deref().unwrap_or("")))?;
            let shown = |severity: &str| severity_rank(severity).unwrap_or(0) >= floor;
            // The colour pass grabs frames, so it takes the sequence read
            // lock for a moment only; work out the times first, then let go.
            let (mut report, color_times, audio_media, to_read, banding_snapshot) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let mut report = klipp_document::qc::check(&seq, &pool, &AnalysedTranscripts);
                let lut = ctx.shared.project.lock().ok().and_then(|p| p.global_lut.clone());
                klipp_document::qc::note_missing_global_lut(&mut report, lut.as_deref());
                // What an 8-bit export renders, frozen here and sampled
                // once the locks are let go (#2074).
                let banding_snapshot = klipp_render::SequenceSnapshot::from_sequence(
                    &seq,
                    &pool,
                    &klipp_core::ExportSettings::default(),
                    lut.clone(),
                    false,
                    klipp_engine::video_decoder::working_colorspace(),
                )
                .ok();
                if loudness {
                    note_loudness_drops(&mut report, &seq, &pool);
                } else {
                    report.findings.push(klipp_document::qc::Finding {
                        severity: klipp_document::qc::Severity::Note,
                        rule: "loudness_unmetered",
                        at_secs: None,
                        what: "the mix was not metered for stretches that drop well under the rest".into(),
                        hint: "check loudness: true meters the mix for section drops before export".into(),
                    });
                }
                // The titles read straight off the picture, for the
                // legibility pass (#1581): up to 16, each once.
                let to_read = klipp_document::qc::titles_to_read(&seq, &pool, 16);
                // Each detected shot of each picture clip, read on its own
                // — a clip holding a cut, or a long walk through changing
                // light, is several shots, and one median across them all
                // judged none of them (#329) — on every visible video
                // track: a short clip on a track above (an overlay, a
                // reveal) used to stand for the whole piece, and the main
                // picture under it went unread (#720). Each clip is read
                // alone, so what is over it is not taken for it (#719).
                // Up to six shots a clip (the longest), fewer when there
                // are many clips, 48 in all.
                // Generators and title cards are pictures too, read and
                // judged as graphics (#1919): a piece made only of them
                // read as zero shots and "nothing amiss".
                let times: Vec<(f64, (f64, f64), klipp_core::ClipId, bool)> = if color { colour_read_times(&seq, &pool) } else { Vec::new() };
                // Each distinct piece of media that carries sound, with a
                // time it plays at — for reading its spectral faults off
                // the analysis (no rendering, so this is always on).
                // With it, the fixes every clip that plays the file's
                // sound already carries (#505).
                // And the role its sound plays: designed (music, an effect,
                // ambience, foley) only while every clip that plays it is
                // (#1907).
                let mut audio_media: Vec<(std::path::PathBuf, String, f64, AudioFixes, SoundUse)> = Vec::new();
                for t in &seq.tracks {
                    for c in t.clips.iter().filter(|c| c.enabled) {
                        let Some(item) = c.media_id().and_then(|mid| pool.get(&mid)) else { continue };
                        if !item.has_audio {
                            continue;
                        }
                        // Heard: a clip on an audio track, or a picture clip
                        // playing its own sound. A picture whose sound is its
                        // linked audio clip is not where the sound is fixed.
                        let heard = t.kind == klipp_timeline::TrackKind::Audio || (c.linked_clip_id.is_none() && c.volume > 0.0);
                        // A sound linked to a picture keeps its settings — the
                        // cleaners among them — on the picture clip, so they
                        // are read there (#725).
                        let fixes = if heard { AudioFixes::on(&sound_owner(&seq, c).audio_effects) } else { AudioFixes::ALL };
                        let used = if heard { SoundUse::of(c.role) } else { SoundUse::Unheard };
                        match audio_media.iter_mut().find(|m| m.0 == item.path) {
                            Some(m) => {
                                m.3 = m.3.and(fixes);
                                m.4 = m.4.and(used);
                            }
                            None => audio_media.push((item.path.clone(), item.filename.clone(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0, fixes, used)),
                        }
                    }
                }
                (report, times, audio_media, to_read, banding_snapshot)
            };
            // Smooth gradients that an 8-bit file will show in steps: a few
            // frames rendered small, as the export would draw them (#2074).
            // Not while a render runs, which it would slow: said, never
            // taken for a clean read.
            let rendering = ctx.shared.exports.lock().is_ok_and(|e| e.iter().any(|j| j.status == "rendering"));
            if rendering && banding_snapshot.is_some() {
                report.findings.push(klipp_document::qc::Finding {
                    severity: klipp_document::qc::Severity::Note,
                    rule: "banding_unchecked",
                    at_secs: None,
                    what: "8-bit banding was not checked: a render is running".into(),
                    hint: "check again once the render is done".into(),
                });
            } else if let Some(found) = banding_snapshot.as_ref().and_then(klipp_render::banding::sample) {
                report.findings.push(klipp_document::qc::Finding {
                    severity: klipp_document::qc::Severity::Note,
                    rule: "banding_8bit",
                    at_secs: Some(found.secs),
                    what: format!("smooth gradients span {} code values over half the frame: an 8-bit file will show them in steps", found.levels),
                    hint: klipp_render::banding::SUGGESTION.into(),
                });
            }
            // Each title's words against the picture under it, that clip
            // rendered alone at the title's middle (#1581). A picture that
            // can't be read is said, never taken for a clean one.
            for title in &to_read {
                match grab_frame_rgba_of(ctx, title.at_secs, Some(title.under), Some(4000)) {
                    Ok((picture, w, h)) => {
                        if let Some(f) = klipp_document::qc::legibility(title, &picture, w as usize, h as usize) {
                            match f.severity {
                                klipp_document::qc::Severity::Error => report.errors += 1,
                                klipp_document::qc::Severity::Warning => report.warnings += 1,
                                klipp_document::qc::Severity::Note => {}
                            }
                            report.findings.push(f);
                        }
                    }
                    Err(e) => report.findings.push(klipp_document::qc::Finding {
                        severity: klipp_document::qc::Severity::Note,
                        rule: "title_hard_to_read",
                        at_secs: Some(title.at_secs),
                        what: format!("{}'s words weren't read against the picture under them ({e})", title.label),
                        hint: "Look at that frame to see they read".into(),
                    }),
                }
            }
            // Every shot read in Lab against the brief's look (or the
            // neutral one, or the screen recording's), faces and all.
            // (severity, secs, what, hint, id): the id names the finding for
            // a review of it (#1932).
            let mut color_findings: Vec<(String, f64, String, String, String)> = Vec::new();
            let mut shots_on_target = 0usize;
            // A survey of many shots reads each once, at its middle; a few
            // are read three times across, so drift within a shot is said.
            let survey = color_times.len() > 8;
            let mut unread: Vec<String> = Vec::new();
            let mut graphics_read = 0usize;
            for (t, span, clip, made) in &color_times {
                // A generated picture is judged as delivered, composited
                // with whatever its blend, opacity or a layer over it
                // makes of it; read alone below, to say whose it is (#1919).
                let read = if *made {
                    measure_at(ctx, *t, None, Some(4000)).map(|m| (vec![(*t, m)], None))
                } else if survey {
                    measure_at(ctx, *t, Some(*clip), Some(4000)).map(|m| (vec![(*t, m)], None))
                } else {
                    measure_clip(ctx, span.0, span.1, Some(*clip), Some(4000))
                };
                let (reads, drift) = match read {
                    Ok(r) => r,
                    Err(e) => {
                        unread.push(format!("{t:.2} s ({e})"));
                        continue;
                    }
                };
                let (_, mut m) = median_measure(&reads);
                let look = if *made { klipp_color::measure::LookTarget::graphics() } else { look_for(ctx, *t) };
                graphics_read += usize::from(*made);
                // A floor the file already holds is the scene's, not the
                // grade's: the file is read only when a finding hangs on it.
                if needs_source(&klipp_color::measure::judge(&m, &look), &look) {
                    m.source = source_reading(ctx, *clip, *t);
                }
                let mut found = colour_report(&m, &look, *t);
                if *made && !found.is_empty() {
                    if let Ok(alone) = measure_at(ctx, *t, Some(*clip), Some(4000)) {
                        found = attribute_to_layer(found, &colour_report(&alone, &look, *t));
                    }
                }
                // The chain's own order, which no measurement can see:
                // a balance behind a look never settles, however many
                // rounds the loop runs.
                {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    if let Some(c) = seq.tracks.iter().flat_map(|tr| tr.clips.iter()).find(|c| c.id == *clip) {
                        for fault in c.own_grade().chain_out_of_order() {
                            found.push(("warning".to_string(), *t, format!("{fault} [grade order]"), "grade_chain shows the chain; the order is input, balance, secondary, look, output".to_string()));
                        }
                    }
                }
                if let Some(d) = drift {
                    found.push(("warning".to_string(), *t, format!("{d} [drift]"), "keyframe the grade across the shot, or cut where the light changes".to_string()));
                }
                if found.is_empty() {
                    shots_on_target += 1;
                }
                // Named by the shot and by what it was judged on, so a
                // review of one holds until the shot changes (#1932).
                let named: Vec<String> = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                    let named = match seq.all_clips().find(|c| c.id == *clip) {
                        Some(c) => {
                            let basis = klipp_document::qc_review::colour_basis(&seq, &pool, c, *span, &colour_management(ctx));
                            found.iter().map(|f| klipp_document::qc_review::colour_finding_id(&colour_rule(&f.2), c, span.0, seq.fps.as_f64(), basis)).collect()
                        }
                        None => found.iter().map(|f| klipp_document::qc_review::finding_id("colour", &colour_rule(&f.2), Some(f.1), seq.fps.as_f64(), &f.2)).collect(),
                    };
                    named
                };
                color_findings.extend(found.into_iter().zip(named).map(|((a, b, c, d), id)| (a, b, c, d, id)));
            }
            let departing: Vec<String> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                seq.grade_groups.iter().filter(|g| g.departs && seq.all_clips().any(|c| c.grade_group == Some(g.id))).map(|g| g.name.clone()).collect()
            };
            // Sound faults: wind, rumble, hum, hiss, from the analysis bands.
            let mut audio_findings: Vec<SoundFinding> = Vec::new();
            for (path, name, at, fixes, used) in &audio_media {
                // A library track or effect is what it is: its low end is
                // not a hum to clean.
                if klipp_analysis::is_licensed_audio(path) {
                    continue;
                }
                if let Some(au) = klipp_analysis::report::MediaAnalysis::load(path).audio {
                    // The analysis reads the file; a fault the clips already
                    // carry the fix for is not asked for again (#505).
                    for (what, fix) in au.faults().into_iter().filter(|(what, _)| !fixes.covers(what)) {
                        audio_findings.push(sound_finding(*at, name, &what, &fix, *used));
                    }
                }
            }

            // Each finding against the sequence's reviews of it (#1932): one
            // kept on purpose leaves the counts for a section of its own,
            // one to be fixed carries its note, and a review whose finding
            // is gone is said, not dropped.
            let (reviews, fps) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                (seq.reviews.clone(), seq.fps.as_f64())
            };
            let craft_ids: Vec<String> = report.findings.iter().map(|f| klipp_document::qc_review::finding_id("check", f.rule, f.at_secs, fps, &f.what)).collect();
            let sound_ids: Vec<String> = audio_findings.iter().map(|f| klipp_document::qc_review::finding_id("sound", "sound", Some(f.at), fps, &format!("{}: {}", f.media, f.what))).collect();
            let all_ids: Vec<String> = craft_ids.iter().chain(color_findings.iter().map(|f| &f.4)).chain(&sound_ids).cloned().collect();
            let matched = klipp_document::qc_review::match_reviews(&all_ids, &reviews);
            let (n_craft, n_colour) = (craft_ids.len(), color_findings.len());
            let accepted_at: std::collections::HashMap<usize, &klipp_timeline::FindingReview> = matched.accepted.iter().copied().collect();
            let to_fix_at: std::collections::HashMap<usize, &klipp_timeline::FindingReview> = matched.to_fix.iter().copied().collect();
            let mut accepted: Vec<serde_json::Value> = Vec::new();
            let mut accepted_lines: Vec<String> = Vec::new();
            let mut keep = |i: usize, severity: &str, at: Option<f64>, what: &str, hint: &mut String| -> bool {
                if let Some(r) = accepted_at.get(&i) {
                    accepted_lines.push(format!("\n[accepted {severity}]{}  {what}. Why: {} (revision {})", at.map(|t| format!(" {t:.2} s")).unwrap_or_default(), r.rationale, r.revision));
                    accepted.push(json!({ "id": all_ids[i], "severity": severity, "at_secs": at, "what": what, "rationale": r.rationale, "revision": r.revision }));
                    return false;
                }
                if let Some(r) = to_fix_at.get(&i) {
                    hint.push_str(&format!(" (to fix: {})", r.rationale));
                }
                true
            };
            let mut craft: Vec<(klipp_document::qc::Finding, String)> = Vec::new();
            for (i, (mut f, id)) in std::mem::take(&mut report.findings).into_iter().zip(craft_ids).enumerate() {
                let severity = match f.severity { klipp_document::qc::Severity::Error => "error", klipp_document::qc::Severity::Warning => "warning", klipp_document::qc::Severity::Note => "note" };
                if keep(i, severity, f.at_secs, &f.what.clone(), &mut f.hint) {
                    craft.push((f, id));
                }
            }
            report.errors = craft.iter().filter(|(f, _)| f.severity == klipp_document::qc::Severity::Error).count();
            report.warnings = craft.iter().filter(|(f, _)| f.severity == klipp_document::qc::Severity::Warning).count();
            report.findings = craft.iter().map(|(f, _)| f.clone()).collect();
            let color_findings: Vec<(String, f64, String, String, String)> = std::mem::take(&mut color_findings).into_iter().enumerate()
                .filter_map(|(i, mut f)| keep(n_craft + i, &f.0.clone(), Some(f.1), &f.2.clone(), &mut f.3).then_some(f)).collect();
            let (audio_findings, sound_ids): (Vec<SoundFinding>, Vec<String>) = std::mem::take(&mut audio_findings).into_iter().zip(sound_ids).enumerate()
                .filter_map(|(i, (mut f, id))| {
                    let what = format!("{}: {}", f.media, f.what);
                    keep(n_craft + n_colour + i, f.severity, Some(f.at), &what, &mut f.hint).then_some((f, id))
                })
                .unzip();
            let lapsed: Vec<&klipp_timeline::FindingReview> = matched.lapsed;

            // The headline counts everything the check found, colour
            // and sound included. It used to count only the craft
            // findings, so a timeline with ten colour errors under it
            // was announced as "0 errors, 0 warnings" and `next` said
            // "Clean. Export." — which is what an agent reads.
            let colour_errors = color_findings.iter().filter(|f| f.0 == "error").count();
            // A shot that could not be read is an error: nothing under it
            // was judged, and the viewer cannot show it either (#2474).
            let errors = report.errors + colour_errors + unread.len();
            let warnings = report.warnings + (color_findings.len() - colour_errors) + audio_findings.iter().filter(|f| f.severity == "warning").count();
            let next = if errors > 0 {
                "Fix the errors before you deliver — each finding says what and where. Then check again."
            } else if warnings > 0 {
                "No errors. Decide each warning: fix it, or keep it on purpose with review_finding, its id and why. Then export."
            } else {
                "Clean. Export at the brief's loudness, poll export_status, then save_project."
            };
            // The craft lines at or over the floor; the counts at the top
            // count them all.
            let mut text = report.text().lines().filter(|l| l.strip_prefix('[').and_then(|r| r.split(']').next()).is_none_or(&shown)).collect::<Vec<_>>().join("\n");
            if errors != report.errors || warnings != report.warnings {
                let notes = report.findings.len() - report.errors - report.warnings;
                let head = format!(
                    "Checked {:.2} s: {errors} error{}, {warnings} warning{}, {notes} note{}.",
                    report.duration_secs,
                    if errors == 1 { "" } else { "s" },
                    if warnings == 1 { "" } else { "s" },
                    if notes == 1 { "" } else { "s" },
                );
                match text.find('\n') {
                    Some(i) => text.replace_range(..i, &head),
                    None => text = head,
                }
            }
            if color {
                let read = color_times.len() - unread.len();
                text.push_str("\n\n");
                text.push_str(&colour_coverage_line(color_times.len(), read - graphics_read, graphics_read, shots_on_target));
                if read == 0 {
                    // Not measured is not clean: nothing more to say here.
                } else if color_findings.is_empty() {
                    text.push_str(" nothing amiss.");
                } else {
                    let listed: Vec<(&str, f64, &str, &str)> = color_findings.iter().filter(|f| shown(&f.0)).map(|f| (f.0.as_str(), f.1, f.2.as_str(), f.3.as_str())).collect();
                    text.push_str(&colour_lines(&listed));
                }
                // A group that leaves the look on purpose is said once, not
                // as a taste finding on every shot of it (#793).
                for name in &departing {
                    text.push_str(&format!("\n[note] grade group \"{name}\" leaves the look on purpose: its shots are judged for faults only (clipping, crushed blacks, faces, noise), not for its colour or contrast"));
                }
                // A shot that could not be read is not one that passed.
                if !unread.is_empty() {
                    text.push_str(&format!("\n[error] {} shot{} could not be read, so are not judged: {}", unread.len(), if unread.len() == 1 { "" } else { "s" }, unread.join("; ")));
                }
            }
            if !audio_findings.is_empty() {
                text.push_str("\n\nSound:");
                for f in audio_findings.iter().filter(|f| shown(f.severity)) {
                    text.push_str(&format!("\n[{}] {}: {}. {}", f.severity, f.media, f.what, f.hint));
                }
            }
            if !accepted_lines.is_empty() {
                text.push_str(&format!("\n\nAccepted on purpose, not counted ({}):", accepted_lines.len()));
                for line in &accepted_lines {
                    text.push_str(line);
                }
            }
            if !lapsed.is_empty() {
                text.push_str("\n\nReviews that no longer hold — what they judged has changed or is gone, so the finding, if it is still there, is counted again:");
                for r in &lapsed {
                    let decision = match r.decision { klipp_timeline::ReviewDecision::Accepted => "accepted", klipp_timeline::ReviewDecision::ToFix => "to fix" };
                    text.push_str(&format!("\n[lapsed, was {decision}]{}  {}: {}", r.at_secs.map(|t| format!(" {t:.2} s")).unwrap_or_default(), r.finding_id, r.rationale));
                }
            }
            // A finding still counted carries its review, a to-fix one's,
            // so a client shows its state without reading the hint (#1978).
            let review_of = |id: &str| reviews.iter().find(|r| r.finding_id == id).map(|r| json!({ "decision": r.decision, "rationale": r.rationale, "revision": r.revision }));
            let craft_json: Vec<serde_json::Value> = craft.iter().map(|(f, id)| {
                let mut v = json!(f);
                v["id"] = json!(id);
                v["review"] = json!(review_of(id));
                v
            }).collect();
            let craft_json: Vec<serde_json::Value> = craft_json.into_iter().filter(|f| shown(f["severity"].as_str().unwrap_or("note"))).collect();
            let below_floor = color_findings.iter().filter(|f| !shown(&f.0)).count() + audio_findings.iter().filter(|f| !shown(f.severity)).count() + craft.iter().filter(|(f, _)| !shown(severity_name(f.severity))).count();
            let findings_json: Vec<serde_json::Value> = color_findings.iter().filter(|f| shown(&f.0)).map(|f| json!({ "id": f.4, "severity": f.0, "rule": "color", "at_secs": f.1, "what": f.2, "hint": f.3, "review": review_of(&f.4) })).collect();
            let audio_json: Vec<serde_json::Value> = audio_findings.iter().zip(&sound_ids).filter(|(f, _)| shown(f.severity)).map(|(f, id)| json!({ "id": id, "severity": f.severity, "rule": "sound", "at_secs": f.at, "media": f.media, "what": f.what, "hint": f.hint, "role": f.role, "review": review_of(id) })).collect();
            if below_floor > 0 {
                text.push_str(&format!("\n\n{below_floor} finding{} under {} not listed, counted above.", if below_floor == 1 { "" } else { "s" }, min_severity.as_deref().unwrap_or("note")));
            }
            Ok(json!({ "text": text, "errors": errors, "warnings": warnings, "below_floor": below_floor, "findings": craft_json, "accepted": accepted, "lapsed_reviews": lapsed, "color_findings": findings_json, "departing_groups": departing, "shots_on_target": shots_on_target, "colour_coverage": if color { json!({ "footage": color_times.len() - unread.len() - graphics_read, "graphics": graphics_read, "unread": unread.len() }) } else { json!(null) }, "sound_findings": audio_json, "duration_secs": report.duration_secs, "revision": ctx.shared.revision(), "next": next }))
        }

        // Said what they did (#741): the step taken back or put back, as
        // the history names it, and what the next one would be.
        which @ (ControlCommand::Undo | ControlCommand::Redo) => {
            let undo = matches!(which, ControlCommand::Undo);
            let labels = || ctx.shared.project.lock().map(|p| (p.undo.clone(), p.redo.clone())).unwrap_or_default();
            let (was_undo, was_redo) = labels();
            let step = if undo { was_undo } else { was_redo };
            let mut v = send(if undo { AppCommand::Undo } else { AppCommand::Redo })?;
            let Some(step) = step else {
                v["text"] = json!(if undo { "Nothing to undo." } else { "Nothing to redo." });
                return Ok(v);
            };
            let (next_undo, next_redo) = labels();
            let mut text = format!("{} \u{201c}{step}\u{201d}.", if undo { "Undid" } else { "Redid" });
            match (&next_undo, &next_redo) {
                (Some(u), _) if undo => text.push_str(&format!(" Next undo would take back \u{201c}{u}\u{201d}.")),
                (_, Some(r)) if !undo => text.push_str(&format!(" Next redo would put back \u{201c}{r}\u{201d}.")),
                _ if undo => text.push_str(" Nothing more to undo."),
                _ => text.push_str(" Nothing more to redo."),
            }
            v[if undo { "undid" } else { "redid" }] = json!(step);
            v["next_undo"] = json!(next_undo);
            v["next_redo"] = json!(next_redo);
            v["text"] = json!(text);
            Ok(v)
        }

        ControlCommand::ReviewFinding { finding_id, decision, rationale } => {
            if in_batch {
                return Err("review_finding is its own undo step; send it on its own".into());
            }
            let rationale = rationale.unwrap_or_default().trim().to_string();
            let decision = match decision.trim() {
                "accepted" => Some(klipp_timeline::ReviewDecision::Accepted),
                "to_fix" => Some(klipp_timeline::ReviewDecision::ToFix),
                "clear" => None,
                other => return Err(format!("decision {other:?} is not one: accepted (kept on purpose), to_fix, or clear")),
            };
            if decision == Some(klipp_timeline::ReviewDecision::Accepted) && rationale.is_empty() {
                return Err("an accepted finding needs a rationale: why it is kept on purpose".into());
            }
            let (old, review) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let Some((_, clip_id, at_secs)) = klipp_document::qc_review::locate(&seq, &finding_id) else {
                    return Err(format!("{finding_id:?} is not a finding id: check gives every finding one"));
                };
                let review = decision.map(|decision| klipp_timeline::FindingReview { finding_id: finding_id.clone(), decision, rationale: rationale.clone(), at_secs, clip_id, revision: ctx.shared.revision() });
                (seq.reviews.clone(), review)
            };
            let mut new: Vec<klipp_timeline::FindingReview> = old.iter().filter(|r| r.finding_id != finding_id).cloned().collect();
            let text = match &review {
                Some(r) if r.decision == klipp_timeline::ReviewDecision::Accepted => format!("Accepted {finding_id} on purpose: {rationale}. Check lists it apart from the faults until what it judged changes."),
                Some(_) => format!("Marked {finding_id} to fix{}.", if rationale.is_empty() { String::new() } else { format!(": {rationale}") }),
                None if new.len() == old.len() => return Err(format!("{finding_id} has no review to clear")),
                None => format!("Cleared the review of {finding_id}."),
            };
            new.extend(review);
            let edit = klipp_document::history::EditCommand::SetReviews { old, new };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Review Finding".into()))?;
            v["text"] = json!(text);
            Ok(v)
        }

        ControlCommand::Batch { commands } => {
            if in_batch {
                return Err("a batch cannot contain a batch".into());
            }
            if commands.is_empty() {
                return Err("an empty batch".into());
            }
            let mut buf = Some(Collected::default());
            for (i, c) in commands.into_iter().enumerate() {
                // Every command checked and translated before any is sent:
                // a bad one refuses the batch whole.
                handle(ctx, c, &mut buf).map_err(|e| format!("batch command {i}: {e}"))?;
            }
            let Collected { commands: cmds, ops } = buf.take().unwrap_or_default();
            if ops.is_empty() {
                return send_envelope(ctx, cmds, true);
            }
            // Looks are resolved edits and apply as one history entry of
            // their own; with other writes beside them the batch would be
            // two undo steps, not the one it promises.
            if !cmds.is_empty() {
                return Err("a batch with looks holds only looks; send the other writes in a batch of their own".into());
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let label = if ops.len() == 1 { "look".to_string() } else { format!("look on {} clips", ops.len()) };
            let mut v = send_full_envelope(ctx, cmds, edits, true, Some(revision), Some(label))?;
            v["text"] = json!(plan.text_folded());
            Ok(v)
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// A severity's rank for check's floor (#2492): note 0, warning 1,
/// error 2; `None` for no severity.
fn severity_rank(name: &str) -> Option<u8> {
    match name.trim().to_ascii_lowercase().as_str() {
        "note" => Some(0),
        "warning" => Some(1),
        "error" => Some(2),
        _ => None,
    }
}

fn severity_name(s: klipp_document::qc::Severity) -> &'static str {
    match s {
        klipp_document::qc::Severity::Error => "error",
        klipp_document::qc::Severity::Warning => "warning",
        klipp_document::qc::Severity::Note => "note",
    }
}

/// The colour findings as lines, grouped by shot (#2492): a shot's time
/// once, its findings under it, and a fix said once already as "as
/// above". Twenty-two shots wrote each time and the same fix on every
/// line, some 63 KB.
pub(crate) fn colour_lines(findings: &[(&str, f64, &str, &str)]) -> String {
    let mut out = String::new();
    let mut said: Vec<&str> = Vec::new();
    let mut at: Option<f64> = None;
    for &(severity, t, what, hint) in findings {
        if at.is_none_or(|a| (a - t).abs() > 1e-6) {
            out.push_str(&format!("\n{t:.2} s:"));
            at = Some(t);
        }
        let fix = if said.contains(&hint) { "Fix as above.".to_string() } else { said.push(hint); hint.to_string() };
        out.push_str(&format!("\n  [{severity}] {what}. {fix}"));
    }
    out
}

#[cfg(test)]
mod colour_lines_tests {
    /// #2492: one line for each shot's time, its findings under it, a
    /// repeated fix said once; the floor's names known.
    #[test]
    fn colour_findings_are_grouped_by_shot() {
        let fix = "temperature until the neutrals read near the target";
        let text = super::colour_lines(&[("warning", 1.0, "cast", fix), ("error", 1.0, "clipped", "lower gain"), ("warning", 4.5, "cast", fix)]);
        assert_eq!(text, format!("\n1.00 s:\n  [warning] cast. {fix}\n  [error] clipped. lower gain\n4.50 s:\n  [warning] cast. Fix as above."));
        assert_eq!((super::severity_rank("Error"), super::severity_rank("note"), super::severity_rank("big")), (Some(2), Some(0), None));
    }
}

/// A stretch of the mix well under the rest of the piece (#1584): a
/// spliced bed left one section 10 LU down and nothing warned, since the
/// check read only the integrated target. Metered a second apart as the
/// export mixes it; a fade over the stretch, or speech in it, makes it a
/// quiet moment meant as one. Asked for (`loudness`), as it mixes the
/// whole piece's sound.
fn note_loudness_drops(report: &mut klipp_document::qc::Report, seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) {
    const STEP: f64 = 1.0;
    let Ok(snapshot) = klipp_render::SequenceSnapshot::from_sequence(seq, pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear) else { return };
    if snapshot.audio_tracks.iter().all(|t| t.clips.is_empty()) {
        return;
    }
    // The whole mix as delivered: its loudness and true peak against the
    // brief's target and the ceiling (#2481).
    if let Ok((integrated, peak)) = mix_delivery(&snapshot) {
        let target = seq.brief.as_ref().and_then(|b| b.loudness_lufs);
        for (severity, rule, what, hint) in crate::grading::delivery_findings(integrated, peak, target, klipp_core::DEFAULT_TRUE_PEAK_DBTP) {
            match severity {
                klipp_document::qc::Severity::Error => report.errors += 1,
                klipp_document::qc::Severity::Warning => report.warnings += 1,
                _ => {}
            }
            report.findings.push(klipp_document::qc::Finding { severity, rule, at_secs: None, what, hint });
        }
    }
    let end = snapshot.end_frame() as f64 / snapshot.fps.max(1.0);
    if end < 2.0 * STEP + 3.0 {
        report.findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.at_secs.unwrap_or(-1.0).total_cmp(&b.at_secs.unwrap_or(-1.0))));
        return;
    }
    let Ok(rows) = mix_loudness(&snapshot, 0.0, end, STEP) else { return };
    let excused = |a: f64, b: f64| {
        let over = |x: f64, y: f64| x < b && y > a;
        seq.tracks.iter().filter(|t| !t.muted).flat_map(|t| t.clips.iter()).filter(|c| c.enabled).any(|c| {
            let (i, o) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
            (c.fade_in_secs > 0.0 && over(i, i + f64::from(c.fade_in_secs)))
                || (c.fade_out_secs > 0.0 && over(o - f64::from(c.fade_out_secs), o))
                || (matches!(c.role, klipp_core::ClipRole::Dialogue | klipp_core::ClipRole::Voiceover) && c.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) && over(i, o))
        })
    };
    for (from, to, lufs, median) in loudness_drops(&rows, STEP, &excused) {
        report.findings.push(klipp_document::qc::Finding {
            severity: klipp_document::qc::Severity::Warning,
            rule: "loudness_drop",
            at_secs: Some(from),
            what: format!("the mix drops to {lufs:.0} LUFS short-term from {from:.1} to {to:.1} s, {:.0} LU under the rest of the piece ({median:.0})", median - lufs),
            hint: "Bring that stretch up (audio sets the quiet clip's level), or splice the bed past it; mix_loudness shows the mix second by second".into(),
        });
        report.warnings += 1;
    }
    // The worst first, then by time, as the check orders the rest.
    report.findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.at_secs.unwrap_or(-1.0).total_cmp(&b.at_secs.unwrap_or(-1.0))));
}

/// The role a file's sound plays across the clips that play it (#1907).
#[derive(Debug, Clone, Copy, PartialEq)]
enum SoundUse {
    /// No clip plays it: its picture's sound is a linked clip's.
    Unheard,
    /// Every clip playing it is music, an effect, ambience or foley.
    Designed(klipp_core::ClipRole),
    /// A clip plays it as dialogue, voice-over or picture sound.
    Speech,
}

impl SoundUse {
    fn of(role: klipp_core::ClipRole) -> Self {
        use klipp_core::ClipRole as R;
        match role {
            R::Music | R::SFX | R::Ambience | R::Foley => Self::Designed(role),
            _ => Self::Speech,
        }
    }

    /// Two clips of one file: speech in either is speech.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Speech, _) | (_, Self::Speech) => Self::Speech,
            (Self::Unheard, x) | (x, Self::Unheard) => x,
            (x, Self::Designed(_)) => x,
        }
    }
}

/// One finding of the check's sound section.
struct SoundFinding {
    severity: &'static str,
    at: f64,
    media: String,
    what: String,
    hint: String,
    role: Option<&'static str>,
}

/// A band fault the analysis found in `media`, judged by the role its
/// sound plays (#1907). Under speech, rumble, hiss and a high floor are
/// faults to clean. In a designed sound they are its character, a synth's
/// bass, an impact's weight, an air sweep's air: a note, worded so, and
/// cleaned only if unwanted. A mains hum is a fault in any role.
fn sound_finding(at: f64, media: &str, what: &str, fix: &str, used: SoundUse) -> SoundFinding {
    let role = match used {
        SoundUse::Designed(r) => Some(match r {
            klipp_core::ClipRole::Music => "music",
            klipp_core::ClipRole::SFX => "sfx",
            klipp_core::ClipRole::Ambience => "ambience",
            _ => "foley",
        }),
        _ => None,
    };
    let hum = what.contains("hum");
    match role.filter(|_| !hum) {
        None => SoundFinding { severity: "warning", at, media: media.to_string(), what: what.to_string(), hint: fix.to_string(), role },
        Some(r) => {
            let character = if what.contains("rumble") {
                "strong low end, likely the sound's own weight or bass"
            } else if what.contains("hiss") {
                "strong high end, likely the sound's own air or sizzle"
            } else {
                "a steady background under it, likely part of the sound"
            };
            let cleaner = if what.contains("rumble") { "wind" } else { "hiss" };
            SoundFinding {
                severity: "note",
                at,
                media: format!("{media} (its clips are {r})"),
                what: character.to_string(),
                hint: format!("Clean it only if it is unwanted (clean_audio {cleaner})"),
                role,
            }
        }
    }
}

/// The moments the colour pass reads, each with its shot's span, the
/// clip and whether it is a made picture (a generator, a card): up to six
/// shots a clip, fewer when there are many clips, 48 in all. A multicam
/// angle is read only where its group cuts to it: elsewhere the program
/// shows another angle, and the angle alone renders black (#2417).
pub(crate) fn colour_read_times(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<(f64, (f64, f64), klipp_core::ClipId, bool)> {
    let clips: Vec<&klipp_timeline::Clip> = seq.tracks.iter()
        .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text) && t.visible)
        .flat_map(|t| t.clips.iter().filter(move |c| (t.kind == klipp_timeline::TrackKind::Video && is_picture_clip(c)) || is_generated_picture(c)))
        .collect();
    let per_clip = (48 / clips.len().max(1)).clamp(1, 6);
    let frame = 1.0 / seq.fps.as_f64().max(1.0);
    let shown = |c: &klipp_timeline::Clip, (a, b): (f64, f64)| -> Vec<(f64, f64)> {
        match seq.multicam_shown_spans(c) {
            None => vec![(a, b)],
            Some(on) => on.into_iter().map(|(x, y)| (a.max(x), b.min(y))).filter(|(x, y)| y - x >= frame).collect(),
        }
    };
    clips.into_iter()
        .flat_map(|c| clip_shot_spans(c, pool, seq.fps.as_f64(), per_clip).into_iter().flat_map(move |span| shown(c, span)).map(move |span| (span, c.id, is_generated_picture(c))))
        .map(|((a, b), id, made)| ((a + b) / 2.0, (a, b), id, made))
        .take(48)
        .collect()
}

#[cfg(test)]
mod sound_role_tests {
    use super::*;
    use klipp_core::ClipRole as R;

    /// #2417: the colour pass reads a multicam angle only where the group
    /// cuts to it, once a stretch; a clip in no group is read as before.
    #[test]
    fn the_colour_pass_reads_what_the_program_shows() {
        use klipp_core::RationalTime;
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/no-analysis-2417.mov".into(), None);
        item.duration = tc(20.0);
        item.video_codec = "hevc".into();
        let media = pool.add(item);
        let group = klipp_core::GroupId::new();
        let mut primary = klipp_timeline::Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts) = (Some(group), 0, vec![(4.0, 1)]);
        let mut second = klipp_timeline::Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        (second.multicam_group, second.multicam_angle) = (Some(group), 1);
        let after = klipp_timeline::Clip::media(media, tc(10.0), tc(12.0), tc(12.0), tc(14.0));
        let (p, s2, a) = (primary.id, second.id, after.id);
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![primary, after];
        let mut v2 = klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video);
        v2.clips = vec![second];
        seq.tracks = vec![v1, v2];
        let times = colour_read_times(&seq, &pool);
        let of = |id| times.iter().filter(|t| t.2 == id).map(|t| t.1).collect::<Vec<_>>();
        let close = |got: Vec<(f64, f64)>, want: (f64, f64)| got.len() == 1 && (got[0].0 - want.0).abs() < 0.05 && (got[0].1 - want.1).abs() < 0.05;
        assert!(close(of(p), (0.0, 4.0)), "angle 0 until the cut: {times:?}");
        assert!(close(of(s2), (4.0, 10.0)), "angle 1 after it: {times:?}");
        assert!(close(of(a), (10.0, 12.0)), "a clip in no group, whole: {times:?}");
        assert_eq!(times.len(), 3, "once a stretch: {times:?}");
    }

    const RUMBLE: &str = "a strong low-frequency rumble — wind or handling — sits under the sound";
    const HISS: &str = "high-frequency hiss or air is loud against the sound";
    const HUM: &str = "a mains hum near 50/60 Hz runs under the sound";

    /// #1907: a designed sound's rumble and hiss are notes on its
    /// character, not faults to clean; under speech they stay warnings with
    /// their fix, and a mains hum is a warning in any role.
    #[test]
    fn a_designed_sounds_low_end_is_a_note_not_a_fault() {
        for role in [R::Music, R::SFX, R::Ambience, R::Foley] {
            for what in [RUMBLE, HISS] {
                let f = sound_finding(1.0, "catalyst.mp3", what, "clean_audio ... else wind", SoundUse::of(role));
                assert_eq!(f.severity, "note", "{role:?} {what}");
                assert!(f.media.contains("its clips are") && f.hint.starts_with("Clean it only if"), "{role:?}: {} {}", f.media, f.hint);
            }
            assert_eq!(sound_finding(1.0, "x.wav", HUM, "clean_audio hum", SoundUse::of(role)).severity, "warning", "hum in {role:?}");
        }
        for role in [R::Dialogue, R::Voiceover, R::BRoll] {
            let f = sound_finding(1.0, "interview.wav", RUMBLE, "clean_audio with voice", SoundUse::of(role));
            assert_eq!((f.severity, f.hint.as_str()), ("warning", "clean_audio with voice"), "{role:?}");
        }
    }

    /// A file one clip plays as music and another as dialogue is speech;
    /// a clip that doesn't play the sound doesn't decide.
    #[test]
    fn speech_in_any_clip_makes_the_file_speech() {
        assert_eq!(SoundUse::of(R::Music).and(SoundUse::of(R::Dialogue)), SoundUse::Speech);
        assert_eq!(SoundUse::Unheard.and(SoundUse::of(R::SFX)), SoundUse::Designed(R::SFX));
        assert_eq!(SoundUse::of(R::Music).and(SoundUse::Unheard), SoundUse::Designed(R::Music));
        assert_eq!(sound_finding(0.0, "x", RUMBLE, "fix", SoundUse::Unheard).severity, "warning");
    }
}
