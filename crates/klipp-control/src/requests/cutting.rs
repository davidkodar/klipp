//! Cutting requests: selects, music and sound effects, beats, style profiles, cut points and cut preparation (#826).

#[allow(unused_imports)]
use crate::*;

pub(crate) fn respond(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();

    match cmd {
        ControlCommand::Selects { media_id, min_gap_secs } => {
            if in_batch {
                return read_in_batch("selects");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(t) = report.transcript.as_ref() else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript yet (analysis: {state}); analyze it first", item.filename));
            };
            let words: Vec<(f64, f64, String)> = t.words.iter().map(|w| (w.start_secs, w.end_secs, w.text.clone())).collect();
            if words.is_empty() {
                return Err(format!("{} has no speech to cut down", item.filename));
            }
            let gap = min_gap_secs.unwrap_or(0.6).max(0.1);
            let selects = suggest_selects(&words, gap, 0.35);
            let keep: Vec<&Select> = selects.iter().filter(|s| !s.filler).collect();
            let kept_secs: f64 = keep.iter().map(|s| s.out_secs - s.in_secs).sum();
            let dur = item.duration.to_secs_f64();
            let mut text = format!("{}: {} selects to keep out of {} runs, {:.1} s of {:.1} s ({:.0}%). Place each keep range, in order, closing the gaps.\n", item.filename, keep.len(), selects.len(), kept_secs, dur, if dur > 0.0 { 100.0 * kept_secs / dur } else { 0.0 });
            for s in &selects {
                let mark = if s.filler { "drop" } else { "keep" };
                let mut line = s.text.clone();
                if line.chars().count() > 80 { line = line.chars().take(79).collect::<String>() + "…"; }
                text.push_str(&format!("  [{mark}] {:.2}-{:.2} s  \"{}\"\n", s.in_secs, s.out_secs, line));
            }
            let (text, cut) = cut_to_budget_with(&text, 12_000, "narrow with a larger min_gap_secs");
            Ok(json!({
                "text": text, "cut": cut, "media_id": media_id, "filename": item.filename,
                "selects": selects.iter().map(|s| json!({ "in_secs": s.in_secs, "out_secs": s.out_secs, "text": s.text, "keep": !s.filler })).collect::<Vec<_>>(),
                "kept_secs": kept_secs, "duration_secs": dur,
                "next": "Place each keep range (place with the clip's source in/out at these times), in order, on the timeline; drop the rest. Then cover any jump cuts and check.",
            }))
        }

        ControlCommand::SearchMusic { query, limit, commercial_only, instrumental, listen } => {
            let listen = listen.unwrap_or(Listen::First(0));
            if in_batch {
                return read_in_batch("search_music");
            }
            if !klipp_audio_search::music_available() {
                return Err("music search is not set up: put jamendo_client_id in ~/.config/klipp/keys.toml (or set KLIPP_JAMENDO_CLIENT_ID)".into());
            }
            let tracks = klipp_audio_search::search_music_filtered(&query, limit.unwrap_or(12), klipp_audio_search::MusicFilter { commercial_only, instrumental })?;
            // The previews asked for heard, side by side, so a candidate is
            // judged before it is imported (#739): the first few, or those
            // named by id (#795).
            let heard = hear_previews(tracks.iter().enumerate().map(|(i, t)| listen.picks(i, &t.id).then(|| t.preview_url.clone())).collect(), listen_to_preview);
            let mut text = format!("{} royalty-free music results for \"{query}\" (Jamendo), best match to the words first:\n", tracks.len());
            for (t, h) in tracks.iter().zip(&heard) {
                let pace = if t.pace.is_empty() { String::new() } else { format!(", {} pace", t.pace) };
                let no_edits = if klipp_audio_search::forbids_edits(&t.license) { " [no edits: ND forbids cutting, fitting or remixing]" } else { "" };
                let preview_only = if t.download_url.is_empty() { " [preview only: add_music takes the preview MP3]" } else { "" };
                text.push_str(&format!("  {} — {}  ({}, {:.0}s{pace}){}{no_edits}{preview_only}  [{}]\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }, t.tags.join(", ")));
                match h {
                    Some(Ok(l)) => text.push_str(&format!("      heard: {}\n", l.describe())),
                    Some(Err(e)) => text.push_str(&format!("      not heard: {e}\n")),
                    None => {}
                }
            }
            let results: Vec<serde_json::Value> = tracks.iter().zip(&heard).map(|(t, h)| {
                let mut v = json!({
                    "title": t.title, "artist": t.artist, "track_id": t.id,
                    "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                    "commercial_ok": t.commercial_ok, "edits_ok": !klipp_audio_search::forbids_edits(&t.license),
                    "preview_url": t.preview_url, "download_url": t.download_url,
                    "page_url": t.page_url, "tags": t.tags, "pace": t.pace,
                });
                if let Some(Ok(l)) = h {
                    v["heard"] = json!({ "bpm": l.bpm, "beat_confidence": l.beat_confidence, "loudness_db": l.loudness_db, "energy_db": l.energy_db, "shape": l.shape });
                }
                v
            }).collect();
            let next = if listen.is_none() {
                "Judge candidates before importing: search again with listen: 3 to hear the first three, or listen: [track_id, …] for those you name (tempo, beat, energy over the track). Add one with add_music (copy its title, artist, download_url, license, license_url, page_url). Honour the licence: non-commercial tracks stay out of commercial work, and the credit is kept for you."
            } else {
                "Pick by the brief's energy: a steady or gently building track sits under a calm piece, a driving one under a fast montage. Add one with add_music (copy its title, artist, download_url, license, license_url, page_url). Honour the licence: non-commercial tracks stay out of commercial work, and the credit is kept for you."
            };
            Ok(json!({ "text": text, "results": results, "next": next }))
        }

        ControlCommand::SearchSfx { query, limit, max_secs, commercial_only, listen } => {
            if in_batch {
                return read_in_batch("search_sfx");
            }
            if !klipp_audio_search::sfx_available() {
                return Err("sound-effect search is not set up: put freesound_token in ~/.config/klipp/keys.toml (or set KLIPP_FREESOUND_TOKEN)".into());
            }
            if max_secs.is_some_and(|m| !(m.is_finite() && m > 0.0)) {
                return Err("max_secs is a length in seconds, above zero".into());
            }
            let listen = listen.unwrap_or(Listen::First(0));
            let found = klipp_audio_search::search_sfx_filtered(&query, limit.unwrap_or(12), klipp_audio_search::SfxFilter { max_secs, commercial_only })?;
            let tracks = found.tracks;
            let heard = hear_previews(tracks.iter().enumerate().map(|(i, t)| listen.picks(i, &t.id).then(|| t.preview_url.clone())).collect(), listen_to_sfx);
            let within = max_secs.map(|m| format!(", {m:.1} s or shorter")).unwrap_or_default();
            let cleared = if commercial_only { ", cleared for commercial use" } else { "" };
            let mut text = format!("{} royalty-free sound effects for \"{query}\" (Freesound){within}{cleared}:\n", tracks.len());
            // Freesound matches every word: say when fewer of them found these (#1573).
            if !tracks.is_empty() && found.matched_query != query.trim() {
                text.push_str(&format!("  (nothing matched all of \"{}\"; these match \"{}\")\n", query.trim(), found.matched_query));
            }
            for (t, h) in tracks.iter().zip(&heard) {
                text.push_str(&format!("  {} — {}  ({}, {:.1}s){}  id {}\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }, t.id));
                match h {
                    Some(Ok(l)) => text.push_str(&format!("      heard: {}\n", l.describe())),
                    Some(Err(e)) => text.push_str(&format!("      not heard: {e}\n")),
                    None => {}
                }
            }
            let results: Vec<serde_json::Value> = tracks.iter().zip(&heard).map(|(t, h)| {
                let mut v = json!({
                    "title": t.title, "artist": t.artist, "track_id": t.id,
                    "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                    "commercial_ok": t.commercial_ok, "preview_url": t.preview_url, "download_url": t.download_url,
                    "page_url": t.page_url, "tags": t.tags,
                });
                if let Some(Ok(l)) = h {
                    v["heard"] = json!({ "secs": l.secs, "peak_secs": l.peak_secs, "loudness_db": l.loudness_db, "peak_db": l.peak_db });
                }
                v
            }).collect();
            Ok(json!({ "text": text, "results": results, "next": "Hear candidates with listen (a count, or [track_id, …]): a sound's length and when it peaks say where it goes, the peak on the cut. Add one with add_music (it takes a sound effect just the same: pass title, artist, preview_url, license, license_url, page_url); it brings Freesound's 128 kbps preview MP3, not the original file. The credit is kept; honour the licence." }))
        }

        ControlCommand::AddMusic { title, artist, track_id, download_url, preview_url, license, license_url, page_url, tracks } => {
            if in_batch {
                return Err("add_music downloads and imports a file; send it on its own, not in a batch (it takes many at once as tracks)".into());
            }
            if tracks.is_empty() {
                return add_one_track(ctx, MusicPick { title, artist, track_id, download_url, preview_url, license, license_url, page_url });
            }
            // Many at once (#1587): each downloaded and imported in turn,
            // each answered on its own; one that fails doesn't stop the
            // rest. Fifteen effects took fifteen calls.
            let mut results = Vec::with_capacity(tracks.len());
            let mut failed = 0;
            for pick in tracks {
                let title = pick.title.clone();
                match add_one_track(ctx, pick) {
                    Ok(v) => results.push(json!({ "title": title, "media_id": v["media_id"], "already_imported": v["already_imported"] == true, "job_id": v["job_id"], "attribution": v["attribution"] })),
                    Err(why) => {
                        failed += 1;
                        results.push(json!({ "title": title, "error": why }));
                    }
                }
            }
            let done = results.len() - failed;
            let text = if failed == 0 { format!("{done} track{} added to the media pool", if done == 1 { "" } else { "s" }) } else { format!("{done} of {} tracks added; {failed} could not be", results.len()) };
            Ok(json!({
                "text": text,
                "added": done,
                "failed": failed,
                "results": results,
                "next": "Place each on an audio track (place marks it music, or sfx for an effect) and duck it under any speech. The credits are saved beside the files for your export notes.",
            }))
        }

        ControlCommand::Beats { media_id, target_secs, skip_intro, at_secs, track } => {
            if in_batch {
                return read_in_batch("beats");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, has_audio, duration) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.has_audio, item.duration.to_secs_f64())
            };
            if !has_audio {
                return Err(format!("{filename} has no sound to find a beat in"));
            }
            if target_secs.is_some_and(|t| !t.is_finite() || t <= 0.0) {
                return Err("target_secs must be a length in seconds above 0".into());
            }
            // Kept in the analysis store too, so the next-step hint can tell
            // whether the cut is on these beats without reading them again.
            let b = klipp_analysis::beats::read_and_cache(std::path::Path::new("ffmpeg"), &path)?;
            let text = if b.confidence < 0.3 {
                format!("{filename}: no clear beat (match {:.0}%). Probably speech or ambience, not music.", b.confidence * 100.0)
            } else {
                format!("{filename}: {:.0} BPM, match {:.0}%, {} beats over the read. Cut or place on a beat time.", b.bpm, b.confidence * 100.0, b.beat_times.len())
            };
            // The grid, not the list: the first beat and the interval say
            // where every beat is, and a few are given to check the drift.
            let first = b.beat_times.first().copied().unwrap_or(0.0);
            let interval: f64 = if b.bpm > 0.0 { 60.0 / b.bpm as f64 } else { 0.0 };
            let first_beats: Vec<f64> = b.beat_times.iter().take(32).map(|t| (t * 1000.0).round() / 1000.0).collect();
            let last = b.beat_times.last().copied().unwrap_or(first);
            let drift = if b.beat_times.len() > 1 && interval > 0.0 { last - (first + interval * (b.beat_times.len() - 1) as f64) } else { 0.0 };
            let text = format!("{text}\nFirst beat at {first:.3} s, then every {interval:.4} s ({:.1} s a bar of four); beat n = {first:.3} + n × {interval:.4}, drifting {drift:+.2} s by the end of the read. The first {} beats: {}", interval * 4.0, first_beats.len(), first_beats.iter().map(|t| format!("{t:.2}")).collect::<Vec<_>>().join(" "));
            let mut v = json!({ "text": text, "media_id": media_id, "bpm": b.bpm, "confidence": b.confidence, "first_beat_secs": first, "beat_interval_secs": interval, "bar_secs": interval * 4.0, "beats_read": b.beat_times.len(), "drift_secs": drift, "first_beats": first_beats, "next": "Cut on beat n = first + n × interval; a shot of two bars is 8 beats. style_profile reads a whole reference at once." });
            // Music: its sections and energy, and a fit to a length (#503).
            if b.confidence >= 0.3 {
                music_shape(&mut v, &path, id, duration, &b, target_secs, skip_intro, at_secs.unwrap_or(0.0), track.as_deref())?;
            } else if target_secs.is_some() {
                return Err(format!("{filename} has no clear beat to fit by bars"));
            }
            Ok(v)
        }

        ControlCommand::StyleProfile { media_id } => {
            if in_batch {
                return read_in_batch("style_profile");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, dur, has_audio, item) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64(), item.has_audio, item.clone())
            };
            let report = klipp_analysis::report::MediaAnalysis::load(&path);
            let Some(shots) = report.shots.as_ref() else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{filename} has no shot analysis yet (analysis: {state}); analyse the reference first, then read its style"));
            };
            let shot_pairs: Vec<(f64, f64)> = shots.shots.iter().map(|s| (s.start_secs, s.end_secs)).collect();
            let sdur = if shots.duration_secs > 0.0 { shots.duration_secs } else { dur };
            let beats = if has_audio {
                klipp_analysis::beats::detect(std::path::Path::new("ffmpeg"), &path, 0.0).unwrap_or(klipp_analysis::beats::BeatData { bpm: 0.0, confidence: 0.0, beat_times: Vec::new() })
            } else {
                klipp_analysis::beats::BeatData { bpm: 0.0, confidence: 0.0, beat_times: Vec::new() }
            };
            let cuts = style_cut_summary(&shot_pairs, &shots.boundaries, sdur, &beats);

            // Look: measure a spread of frames off the file through the scopes.
            let n = 8usize;
            let (mut black, mut mid, mut white, mut sat, mut warm, mut cnt) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            let mut measures: Vec<klipp_color::measure::FrameMeasure> = Vec::new();
            for i in 0..n {
                let t = (i as f64 + 0.5) / n as f64 * dur;
                if let Ok((data, w, h)) = decode_frame_rgba(&path, t) {
                    let faces: Vec<[f32; 4]> = report.framing.as_ref().and_then(|f| f.people_at(t, 1.5).map(|s| face_or_head_regions(f, s))).unwrap_or_default();
                    measures.push(klipp_color::measure::measure_frame(&data, w, h, &faces));
                    let st = frame_stats(&data, w, h, t);
                    let g = |a: &str, b: &str| st.get(a).and_then(|x| x.get(b)).and_then(|x| x.as_f64()).unwrap_or(0.0);
                    black += g("luma_ire", "black");
                    mid += g("luma_ire", "mid");
                    white += g("luma_ire", "white");
                    sat += st.get("saturation_pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    warm += g("rgb_balance", "red") - g("rgb_balance", "blue");
                    cnt += 1.0;
                }
            }
            let look = if cnt > 0.0 {
                let (black, mid, white, sat, warm) = (black / cnt, mid / cnt, white / cnt, sat / cnt, warm / cnt * 100.0);
                let contrast = white - black;
                let tone = if warm > 3.0 { "warm" } else if warm < -3.0 { "cool" } else { "neutral" };
                let punch = if contrast > 75.0 { "high-contrast" } else if contrast < 45.0 { "flat" } else { "medium-contrast" };
                let col = if sat > 45.0 { "saturated" } else if sat < 20.0 { "muted" } else { "natural" };
                json!({ "black_ire": black.round(), "mid_ire": mid.round(), "white_ire": white.round(), "contrast_ire": contrast.round(), "saturation_pct": sat.round(), "warmth": (warm.round()), "summary": format!("{tone}, {punch}, {col}"), "look_target": klipp_color::measure::LookTarget::from_measures(&format!("like {filename}"), &measures) })
            } else {
                json!(null)
            };
            let mut response = style_profile_response(&filename, &media_id, cuts, &beats, look,
                StylePacingInput { shots, motion: report.motion.as_ref(), sheet: report.screen_beatsheet(&item) });
            if report.motion.as_ref().is_none_or(|m| m.overall_mean().is_none())
                && klipp_analysis::service::status(id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running)) {
                response["next"] = json!("Motion analysis is queued or running; wait for it to finish, then read style_profile again. Do not queue a second analysis.");
                for pointer in ["/text", "/guidance", "/pacing_evidence/guidance"] {
                    if let Some(value)=response.pointer_mut(pointer) {
                        if let Some(text)=value.as_str() {
                            *value=json!(text.replace("Re-analyse the reference and check that its video stream decodes.", "Motion analysis is still in progress; wait for completion.")
                                .replace("Re-analyse the reference to populate missing or outdated analysis.", "Motion analysis is still in progress; wait for completion."));
                        }
                    }
                }
            }
            Ok(response)
        }

        ControlCommand::CutPoints { media_id, near_secs, from_secs, to_secs } => {
            if in_batch {
                return read_in_batch("cut_points");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let dur = item.duration.to_secs_f64();
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(dur));
            let words: Vec<(f64, f64, String)> = report.transcript.as_ref().map(|t| t.words.iter().map(|w| (w.start_secs, w.end_secs, w.text.clone())).collect()).unwrap_or_default();
            let segments: Vec<(f64, f64, String)> = report.transcript.as_ref().map(|t| t.segments.iter().map(|s| (s.start_secs, s.end_secs, s.text.clone())).collect()).unwrap_or_default();
            let silences: Vec<(f64, f64)> = report.audio.as_ref().map(|au| au.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default();
            let shots: Vec<f64> = report.shots.as_ref().map(|s| s.boundaries.clone()).unwrap_or_default();
            if words.is_empty() && segments.is_empty() && silences.is_empty() && shots.is_empty() {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript, silence or shot analysis yet (analysis: {state}); analyze it, and see analysis_status", item.filename));
            }
            let mut v = cut_points(&words, &segments, &silences, &shots, near_secs, (a, b));
            v["media_id"] = json!(media_id);
            v["filename"] = json!(item.filename);
            v["next"] = json!("Cut on a strength-3 pause where you can; a strength-2 full stop or shot change is clean too. Trim or place to the point's secs. Then check.");
            Ok(v)
        }

        ControlCommand::PrepareCut { from_secs, to_secs, near_secs, intent, max_candidates } => {
            if in_batch { return read_in_batch("prepare_cut"); }
            cut_prep::prepare(ctx, from_secs, to_secs, near_secs, intent, max_candidates)
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// How long a fitted track's join crossfades: short enough to keep the
/// beat, long enough that a loud bar into a quiet one isn't a click.
const JOIN_CROSSFADE_SECS: f64 = 0.25;

/// A music track's sections and energy over its whole length, added to
/// a beats read (#503), and with `target` a fit to that length by whole
/// bars, as place ops from `at` on `track`.
#[allow(clippy::too_many_arguments)]
fn music_shape(v: &mut serde_json::Value, path: &std::path::Path, id: klipp_core::MediaId, duration: f64, beats: &klipp_analysis::beats::BeatData, target: Option<f64>, skip_intro: bool, at: f64, track: Option<&str>) -> Result<(), String> {
    use klipp_analysis::music_structure as music;
    const SR: u32 = 22_050;
    let pcm = klipp_analysis::audio_sync::decode_mono_pcm(std::path::Path::new("ffmpeg"), path, SR, 0.0, duration.clamp(1.0, 1800.0))?;
    let Some(shape) = music::analyse(&pcm, SR, beats) else { return Ok(()) };
    let kind = |k: music::SectionKind| serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let said: Vec<String> = shape.sections.iter().map(|s| format!("{} {:.1}-{:.1} s (energy {:.0}%)", kind(s.kind), s.start_secs, s.end_secs, s.energy * 100.0)).collect();
    let mut text = v["text"].as_str().unwrap_or_default().to_string();
    text.push_str(&format!("\nSections, on phrases of four bars: {}.", said.join("; ")));
    v["sections"] = json!(shape.sections);
    v["bar_lines"] = json!(shape.bars.iter().map(|t| (t * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    v["bar_energy"] = json!(shape.energy.iter().map(|e| (e * 100.0).round() / 100.0).collect::<Vec<_>>());
    if let Some(target) = target {
        let from = if skip_intro { music::intro_end(&shape) } else { None };
        let fitted = match from { Some(start) => music::fit_from(&shape, target, start), None => music::fit(&shape, target) };
        let fit = fitted.ok_or_else(|| format!("the track ({duration:.1} s) cannot be fitted to {target:.1} s by whole bars: it needs a first and last phrase and room between"))?;
        match (skip_intro, from) {
            (true, Some(start)) => text.push_str(&format!("\nThe intro (0.0-{start:.1} s) is left out: the fit starts on the music.")),
            (true, None) => text.push_str("\nThe track has no intro section to leave out; fitted from its start."),
            _ => {}
        }
        let track_ref = match track {
            Some(t) => match t.parse::<usize>() {
                Ok(i) => klipp_document::ops::TrackRef::Index(i),
                Err(_) => klipp_document::ops::TrackRef::Kind(t.to_string()),
            },
            None => klipp_document::ops::TrackRef::Kind("audio".into()),
        };
        let mut place_at = at;
        let mut ops: Vec<serde_json::Value> = fit
            .segments
            .iter()
            .map(|&(a, b)| {
                let op = klipp_document::ops::Op::Place { media_id: id, track: track_ref.clone(), at: klipp_document::ops::Anchor::Secs(place_at), source_in: Some(a), source_out: Some(b), insert: false, with_audio: true };
                place_at += b - a;
                serde_json::to_value(op).unwrap_or_default()
            })
            .collect();
        // Each join crossfaded (#1575): on an audio track a transition is a
        // constant-power crossfade into both clips' handles, named by the
        // clip its place step made ("@N").
        let joins = fit.segments.len().saturating_sub(1);
        ops.extend((0..joins).map(|n| {
            let op = klipp_document::ops::Op::Transition { clip_id: klipp_document::ops::ClipRef::Made(klipp_document::ops::StepRef(n)), kind: "cross_dissolve".into(), secs: JOIN_CROSSFADE_SECS, remove: false };
            serde_json::to_value(op).unwrap_or_default()
        }));
        let how = match fit.bars_changed {
            0 => "already that long".to_string(),
            n if n < 0 => format!("{} bars taken out", -n),
            n => format!("{n} bars repeated"),
        };
        text.push_str(&format!(
            "\nFitted to {:.2} s ({how}{}): play {} back to back. Apply the ops as they are, in one apply: they place it{}.",
            fit.length_secs,
            fit.join_secs.map(|j| format!(", the join at {j:.2} s")).unwrap_or_default(),
            fit.segments.iter().map(|(a, b)| format!("{a:.2}-{b:.2} s")).collect::<Vec<_>>().join(" then "),
            if joins > 0 { format!(" and crossfade {} over {JOIN_CROSSFADE_SECS:.2} s", if joins == 1 { "the join" } else { "each join" }) } else { String::new() }
        ));
        v["fit"] = json!(fit);
        v["ops"] = json!(ops);
        v["next"] = json!("apply the ops as they are, in one apply: they place the fitted track and crossfade its joins");
    }
    v["text"] = json!(text);
    Ok(())
}

/// Download one searched track and import it, its licence and credit
/// kept beside the file.
fn add_one_track(ctx: &ControlContext, pick: MusicPick) -> Result<serde_json::Value, String> {
    let MusicPick { title, artist, track_id, download_url, preview_url, license, license_url, page_url } = pick;
    let track = klipp_audio_search::Track {
        source: klipp_audio_search::Source::JamendoMusic,
        id: track_id, title: title.clone(), artist: artist.clone(),
        duration_secs: 0.0, license: license.clone(), license_url,
        commercial_ok: true, preview_url, download_url, page_url, image_url: String::new(), tags: Vec::new(), pace: String::new(),
    };
    let path = klipp_audio_search::download_track(&track, &klipp_audio_search::download_dir())?;
    let mut v = handle(ctx, ControlCommand::import_one(path.to_string_lossy().to_string(), true), &mut None)?;
    v["text"] = json!(format!("Added \"{title}\" by {artist} ({license}) to the media pool. Place it on an audio track (place marks it music) and duck it under speech.", license = if license.is_empty() { "licence with the file".to_string() } else { license }));
    v["attribution"] = json!(track.attribution());
    v["next"] = json!("Place it on an audio track (place marks it music, or sfx for an effect) and duck it under any speech. The credit is saved beside the file for your export notes.");
    Ok(v)
}
