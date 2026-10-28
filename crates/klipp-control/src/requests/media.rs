//! Media requests: the timeline view, the media pool, importing and relinking, multicam, and markers (#826).

#[allow(unused_imports)]
use crate::*;

/// The pool's offline files, for the replies that say how a project
/// stands (#2480): how many, how many the project's sequences use (the
/// open one and the others, #2790), the first few names, and what finds
/// them. `None` when every file is there.
pub(crate) fn offline_media(ctx: &ControlContext) -> Option<serde_json::Value> {
    // Taken and let go before the sequence is read: the rest of the
    // socket reads the sequence first, and holding both the other way
    // round could deadlock against a writer.
    let offline: Vec<(klipp_core::MediaId, String)> = {
        let pool = ctx.media_pool.read().ok()?;
        pool.all().filter(|m| m.offline).map(|m| (m.id, m.filename.clone())).collect()
    };
    if offline.is_empty() {
        return None;
    }
    let used = {
        let seq = ctx.sequence.read().ok()?;
        let others = ctx.others.read().ok()?;
        let ids: Vec<klipp_core::MediaId> = offline.iter().map(|(id, _)| *id).collect();
        offline_in_use(&ids, std::iter::once(&*seq).chain(others.iter().map(|s| &*s.sequence)))
    };
    let mut names: Vec<&str> = offline.iter().map(|(_, name)| name.as_str()).collect();
    names.sort_unstable();
    let shown = names.iter().take(5).map(|n| quoted(n)).collect::<Vec<_>>().join(", ");
    let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
    let text = format!(
        "{} media file(s) are offline (moved or missing), {used} of them used in the project's sequences: {shown}{more}. relink_media with folder finds them there by content, name, reel, timecode and length.",
        offline.len()
    );
    Some(json!({ "count": offline.len(), "on_timeline": used, "files": names, "text": text }))
}

/// How many of the `offline` files the `sequences` use, each counted once
/// (#2790): the open one and the others, as a relink brings them back to
/// every one.
fn offline_in_use<'a>(offline: &[klipp_core::MediaId], sequences: impl Iterator<Item = &'a klipp_timeline::Sequence>) -> usize {
    let in_use: std::collections::HashSet<klipp_core::MediaId> = sequences.flat_map(|s| s.tracks.iter().flat_map(|t| &t.clips)).filter_map(|c| c.media_id()).collect();
    offline.iter().filter(|id| in_use.contains(id)).count()
}

#[cfg(test)]
mod offline_tests {
    use super::*;

    /// #2790: a file offline that only another sequence uses is counted;
    /// one no sequence uses is not, nor one used twice counted twice.
    #[test]
    fn offline_files_are_counted_over_every_sequence() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
        let (a, b, c) = (klipp_core::MediaId::new(), klipp_core::MediaId::new(), klipp_core::MediaId::new());
        let with = |media: &[klipp_core::MediaId]| {
            let mut seq = klipp_timeline::Sequence::new_default();
            seq.tracks[0].clips = media.iter().enumerate().map(|(i, m)| klipp_timeline::Clip::media(*m, tc(i as f64), tc(i as f64 + 1.0), tc(0.0), tc(1.0))).collect();
            seq
        };
        let (open, other) = (with(&[a]), with(&[b, a]));
        assert_eq!(offline_in_use(&[a, b, c], [&open].into_iter()), 1, "the open sequence alone");
        assert_eq!(offline_in_use(&[a, b, c], [&open, &other].into_iter()), 2, "every sequence, each file once");
    }
}

pub(crate) fn respond(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();
    let mut send = sender!(ctx, collect);

    match cmd {
        ControlCommand::Timeline { max_chars, track, from_secs, to_secs } => {
            if in_batch {
                return read_in_batch("timeline");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            if let Some(t) = track.filter(|t| *t >= seq.tracks.len()) {
                return Err(format!("no track {t}: the sequence has {} (0 is the top)", seq.tracks.len()));
            }
            if let (Some(a), Some(b)) = (from_secs, to_secs) {
                if b <= a {
                    return Err(format!("from_secs {a:.2} is not before to_secs {b:.2}"));
                }
            }
            let presented = ctx.frame_buffer.lock().ok().and_then(|fb| fb.shown_presented()).map(|t| t.to_secs_f64());
            let view = TimelineView { track, from_secs, to_secs };
            let full = timeline_text(&seq.with_own_grades(), &pool, presented, ctx.shared.revision(), view);
            let (text, cut) = cut_to_budget(&full, max_chars.unwrap_or(12_000));
            Ok(json!({ "text": text, "cut": cut, "chars": full.len(), "revision": ctx.shared.revision() }))
        }

        ControlCommand::RelinkMedia { media_id, path: None, folder: Some(folder), .. } => {
            if in_batch {
                return Err("a folder relink searches the disk; send it on its own".into());
            }
            let dir = std::path::PathBuf::from(&folder);
            if !dir.is_absolute() || !dir.is_dir() {
                return Err(format!("no folder at {folder:?} (give an absolute path)"));
            }
            let only = media_id.as_deref().map(|m| parse_uuid(m, "media_id").map(klipp_core::MediaId)).transpose()?;
            let wanted: Vec<klipp_media::relink::Wanted> = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                match only {
                    Some(id) => vec![klipp_media::relink::Wanted::from_item(pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?)],
                    None => pool.all().filter(|m| m.offline).map(klipp_media::relink::Wanted::from_item).collect(),
                }
            };
            if wanted.is_empty() {
                return Ok(json!({ "relinked": [], "text": "No media is offline: nothing to look for." }));
            }
            let criteria = klipp_media::relink::Criteria::default();
            let found = klipp_media::relink::scan(&dir, &wanted, criteria, &std::sync::atomic::AtomicBool::new(false), |_| {});
            let proposals = klipp_media::relink::propose(&wanted, &found.candidates, criteria);
            let (mut relinked, mut choose, mut missing, mut commands) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for (w, proposal) in wanted.iter().zip(proposals) {
                let path_of = |i: usize| found.candidates[i].path.to_string_lossy().into_owned();
                let id = w.media_id.0.to_string();
                match proposal {
                    // Only evidence enough to be sure relinks unasked: a
                    // name alone is a suggestion.
                    klipp_media::relink::Proposal::Found(s) if s.is_strong() => {
                        let p = found.candidates[s.candidate].path.clone();
                        commands.push(AppCommand::RelinkMedia { media_id: w.media_id, new_path: p });
                        relinked.push(json!({ "media_id": id, "file": w.filename, "path": path_of(s.candidate), "matched": s.matched.iter().map(|c| c.label()).collect::<Vec<_>>(), "differences": s.warnings }));
                    }
                    klipp_media::relink::Proposal::Found(s) => choose.push(json!({ "media_id": id, "file": w.filename, "candidates": [path_of(s.candidate)], "why": format!("only its {} match", s.matched.iter().map(|c| c.label()).collect::<Vec<_>>().join(" and ")) })),
                    klipp_media::relink::Proposal::Ambiguous(hits) => choose.push(json!({ "media_id": id, "file": w.filename, "candidates": hits.iter().map(|h| path_of(h.candidate)).collect::<Vec<_>>(), "why": "several files match it equally" })),
                    klipp_media::relink::Proposal::NotFound { rejected } => missing.push(json!({ "media_id": id, "file": w.filename, "rejected": rejected })),
                }
            }
            if !commands.is_empty() {
                send_envelope(ctx, commands, false)?;
            }
            let mut text = format!("Searched {folder} ({} media files): {} of {} relinked", found.candidates.len(), relinked.len(), wanted.len());
            if !choose.is_empty() {
                text.push_str(&format!("; {} to choose (relink_media with media_id and path)", choose.len()));
            }
            if !missing.is_empty() {
                text.push_str(&format!("; {} not found there", missing.len()));
            }
            if found.hit_file_limit {
                text.push_str("; the search stopped at its file limit, so pick a folder nearer the footage");
            }
            Ok(json!({ "relinked": relinked, "choose": choose, "not_found": missing, "files_searched": found.candidates.len(), "text": text }))
        }

        ControlCommand::RelinkMedia { media_id, path, force, .. } => {
            let (Some(media_id), Some(path)) = (media_id, path) else {
                return Err("give media_id and path for one file, or folder to look for the offline media in".into());
            };
            let id = klipp_core::MediaId(parse_uuid(&media_id, "media_id")?);
            let p = std::path::PathBuf::from(&path);
            if !p.is_absolute() || !p.is_file() {
                return Err(format!("no file at {path:?} (give an absolute path)"));
            }
            // What the project knew of the item, judged against the file
            // before relinking, by the rules a relink search uses: the
            // answer says whether it is the same clip.
            let wanted = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_media::relink::Wanted::from_item(pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?)
            };
            let verdict = klipp_media::relink::check_pick(&wanted, &p);
            use klipp_media::relink::PickVerdict;
            if let (PickVerdict::Contradicted(why), false) = (&verdict, force) {
                return Err(format!("{path} is probably not the same clip: {why}. Nothing was relinked; send force: true to relink it anyway"));
            }
            let mut v = send(AppCommand::RelinkMedia { media_id: id, new_path: p })?;
            v["check"] = match verdict {
                PickVerdict::Confirmed(w) => json!({ "verdict": "same clip", "differences": w }),
                PickVerdict::Unconfirmed(w) => json!({ "verdict": "nothing ties it to the clip, nothing against it", "differences": w }),
                PickVerdict::Contradicted(why) => json!({ "verdict": "probably not the same clip", "why": why }),
            };
            Ok(v)
        }

        ControlCommand::MakeProxies { media_ids, use_proxies } => {
            if in_batch {
                return Err("make_proxies starts a job; send it on its own".into());
            }
            // The files asked for, or every picture file the timeline
            // shows; one with its proxy made already is left be.
            // The sequence read and let go before the pool is: the rest of
            // the socket reads them in that order.
            let ids: Vec<klipp_core::MediaId> = if media_ids.is_empty() {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let mut on: Vec<klipp_core::MediaId> = seq.tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.media_id()).collect();
                on.sort_by_key(|id| id.0);
                on.dedup();
                on
            } else {
                media_ids.iter().map(|m| parse_uuid(m, "media_id").map(klipp_core::MediaId)).collect::<Result<_, _>>()?
            };
            let (wanted, ready) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let mut wanted = Vec::new();
                let mut ready = 0;
                for id in ids {
                    let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                    if item.video_codec.is_empty() || item.is_still || item.kind != klipp_media::MediaItemKind::File {
                        if !media_ids.is_empty() {
                            return Err(format!("{} has no moving picture to make a proxy of", item.filename));
                        }
                        continue;
                    }
                    if item.offline {
                        return Err(format!("{} is offline: relink it first", item.filename));
                    }
                    if item.proxy_ready { ready += 1 } else { wanted.push(id) }
                }
                (wanted, ready)
            };
            let mut commands: Vec<AppCommand> = wanted.iter().map(|id| AppCommand::GenerateProxy { media_id: *id }).collect();
            if let Some(on) = use_proxies {
                commands.push(AppCommand::SetUseProxies(on));
            }
            if commands.is_empty() {
                return Ok(json!({ "made": 0, "ready": ready, "text": format!("{ready} file(s) have their proxies already; nothing to make") }));
            }
            let mut v = send_envelope(ctx, commands, false)?;
            let job_id = format!("proxies-{}", uuid::Uuid::new_v4());
            if !wanted.is_empty() {
                jobs::register_proxies(&job_id, wanted.clone());
                v["job_id"] = json!(job_id);
            }
            let using = match use_proxies {
                Some(true) => " Playback reads proxies where a file has one.",
                Some(false) => " Playback reads the originals.",
                None => "",
            };
            v["making"] = json!(wanted.len());
            v["ready"] = json!(ready);
            v["text"] = json!(if wanted.is_empty() {
                format!("{ready} file(s) have their proxies already.{using}")
            } else {
                format!("Making {} prox{} ({ready} ready already) as job {job_id}; follow it with job_status.{using}", wanted.len(), if wanted.len() == 1 { "y" } else { "ies" })
            });
            Ok(v)
        }

        ControlCommand::TagMedia { media_id, add, remove } => {
            let id = klipp_core::MediaId(parse_uuid(&media_id, "media_id")?);
            let tags = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?.tags.clone()
            };
            let mut commands = Vec::new();
            // Removed by index, highest first, so the rest keep theirs.
            let mut gone: Vec<usize> = remove.iter().filter_map(|t| tags.iter().position(|x| x == t)).collect();
            gone.sort_unstable_by(|a, b| b.cmp(a));
            gone.dedup();
            commands.extend(gone.into_iter().map(|tag_index| AppCommand::RemoveMediaTag { media_id: id, tag_index }));
            commands.extend(add.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).map(|tag| AppCommand::AddMediaTag { media_id: id, tag }));
            if commands.is_empty() {
                return Err("nothing to change: give tags to add or remove (removing needs a tag the item has)".into());
            }
            send_envelope(ctx, commands, true)
        }

        ControlCommand::ListMedia => {
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            Ok(json!({
                "items": pool.all().map(|m| {
                    let artwork = (media_kind(m) == "audio" && !m.offline).then(|| klipp_media::artwork_size(&m.path)).flatten();
                    media_listing(m, artwork)
                }).collect::<Vec<_>>(),
            }))
        }

        ControlCommand::ImportMedia { path, paths, folder, analyze } => {
            // Many files: each imported as one would be, each answered on
            // its own, and a file that can't be doesn't stop the rest
            // (#1587: 42 shots took a 42-entry batch).
            if !paths.is_empty() || folder.is_some() {
                let mut files: Vec<String> = (!path.is_empty()).then_some(path).into_iter().chain(paths).collect();
                if let Some(dir) = folder {
                    files.extend(media_in_folder(std::path::Path::new(&dir))?);
                }
                if files.is_empty() {
                    return Err("no media file to import: the folder holds none".into());
                }
                let mut results = Vec::with_capacity(files.len());
                let mut failed = 0;
                for file in &files {
                    match handle(ctx, ControlCommand::import_one(file.clone(), analyze), &mut None) {
                        Ok(v) => results.push(json!({ "path": file, "media_id": v["media_id"], "already_imported": v["already_imported"] == true, "job_id": v["job_id"] })),
                        Err(why) => {
                            failed += 1;
                            results.push(json!({ "path": file, "error": why }));
                        }
                    }
                }
                let done = files.len() - failed;
                let text = if failed == 0 { format!("{done} file{} imported", if done == 1 { "" } else { "s" }) } else { format!("{done} of {} files imported; {failed} could not be", files.len()) };
                return Ok(json!({
                    "text": text,
                    "imported": done,
                    "failed": failed,
                    "results": results,
                    "analysis_requested": analyze,
                    "next": "A result with no media_id is still probing: follow its job_id. Read transcript and shots only after analysis completes.",
                }));
            }
            if path.is_empty() {
                return Err("import_media needs a path, paths or a folder".into());
            }
            let p = std::path::PathBuf::from(&path);
            if !p.exists() {
                return Err(format!("file not found: {path}"));
            }
            // A file already in the pool is not imported again: the second
            // copy would be a second id for the same footage.
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            if let Some(item) = pool.find_by_path(&p) {
                return Ok(json!({ "already_imported": true, "media_id": item.id.0.to_string(), "filename": item.filename }));
            }
            drop(pool);
            // A file ffmpeg cannot open is refused here, with its reason.
            // Sent on, the import failed in the engine, only the log said
            // so, and this replied ok with no media id (#654).
            if let Err(why) = klipp_media::readable(&p) {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(path.clone());
                return Err(format!("{name} could not be imported: {why}. Is it a complete video, audio or image file?"));
            }
            let mut v = send(if analyze { AppCommand::ImportMedia { path: p.clone() } } else { AppCommand::ImportMediaWithAnalysis { path: p.clone(), analyze } })?;
            if let Some(operation_id) = v["operation_id"].as_str().map(str::to_owned) {
                jobs::register_import(ctx, &operation_id, p.clone(), analyze);
                v["job_id"] = json!(operation_id);
            }
            // The id it just made, waited for. Without this the first
            // call answers with no media_id at all and only a *second*
            // one finds the path in the pool — so an agent importing a
            // batch gets a list of nothing and has to ask again for
            // every file.
            match if v["applied"] == true { imported_id_within(ctx, &p, std::time::Duration::from_secs(10)) } else { None } {
                Some(id) => {
                    if let (Some(operation_id), Ok(uuid)) = (v["operation_id"].as_str(), id.parse()) {
                        jobs::note_imported(operation_id, klipp_core::MediaId(uuid));
                    }
                    v["media_id"] = json!(id);
                }
                None => {
                    v["note"] = json!("the import is queued or still probing; follow job_id until the media ID appears");
                }
            }
            v["analysis_requested"] = json!(analyze);
            if v["applied"] != true {
                v["next"] = json!("The UI has not acknowledged import yet. Resolve operation_id first, then follow job_id for the media pool result.");
                return Ok(v);
            }
            if !analyze {
                v["next"] = json!("Follow job_id until import completes. Call analyze with the resulting media_id if analysis is needed.");
                return Ok(v);
            }
            v["next"] = json!("Follow job_id until media enters the pool, then follow its analysis_job_id for analysis progress. Read transcript and shots only after analysis completes.");
            Ok(v)
        }

        ControlCommand::CreateMulticam { media_ids, sync } => {
            let method = match sync.as_deref().map(str::trim) {
                None | Some("") | Some("timecode_then_audio") => klipp_core::MulticamSync::TimecodeThenAudio,
                Some("timecode") => klipp_core::MulticamSync::Timecode,
                Some("audio") => klipp_core::MulticamSync::Audio,
                Some(other) => return Err(format!("no sync method \"{other}\": timecode, audio or timecode_then_audio")),
            };
            let ids = media_ids.iter().map(|s| parse_uuid(s, "media_id").map(klipp_core::MediaId)).collect::<Result<Vec<_>, _>>()?;
            if ids.len() < 2 {
                return Err("a multicam needs two angles or more".into());
            }
            {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                if let Some(missing) = ids.iter().find(|id| pool.get(id).is_none()) {
                    return Err(format!("no media item {}", missing.0));
                }
            }
            let before_groups = jobs::existing_multicam_groups(ctx);
            let requested_ids: Vec<_> = ids.iter().map(|id| id.0.to_string()).collect();
            let mut v = send(AppCommand::CreateSyncedMulticam { media_ids: ids, method })?;
            if let Some(operation_id) = v["operation_id"].as_str().map(str::to_owned) {
                jobs::register_sync(ctx, &operation_id, requested_ids, before_groups);
                v["job_id"] = json!(operation_id);
            }
            v["next"] = json!(if v["applied"] == true {
                "Sync is running. Follow job_id until the new multicam group appears, then switch_angle to cut."
            } else {
                "The UI has not acknowledged sync yet. Resolve operation_id first, then follow job_id."
            });
            Ok(v)
        }

        ControlCommand::SwitchAngle { angle, at_secs } => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let at = match at_secs {
                Some(s) => klipp_core::Timecode::from_secs_f64(s.max(0.0), seq.fps.rate()),
                None => {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    fb.shown_presented().ok_or("no playhead yet: give at_secs")?
                }
            };
            let m = seq.multicam_at(None, at).ok_or_else(|| format!("no multicam group at {:.2} s", at.to_secs_f64()))?;
            let wanted = angle.checked_sub(1).and_then(|a| u8::try_from(a).ok());
            if !wanted.is_some_and(|w| m.angles.contains(&w)) {
                let have: Vec<usize> = m.angles.iter().map(|a| *a as usize + 1).collect();
                return Err(format!("the group has no angle {angle}; it has {have:?}"));
            }
            drop(seq);
            send(AppCommand::SwitchMulticamAngleAt { angle, at })
        }

        ControlCommand::SetMulticamSound { sound, at_secs } => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let at = match at_secs {
                Some(s) => klipp_core::Timecode::from_secs_f64(s.max(0.0), seq.fps.rate()),
                None => {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    fb.shown_presented().ok_or("no playhead yet: give at_secs")?
                }
            };
            let m = seq.multicam_at(None, at).ok_or_else(|| format!("no multicam group at {:.2} s", at.to_secs_f64()))?;
            let have: Vec<usize> = m.angles.iter().map(|a| *a as usize + 1).collect();
            let sound = match sound.trim() {
                "follow" => klipp_core::MulticamSound::FollowPicture,
                "off" => klipp_core::MulticamSound::Off,
                n => {
                    let a = n.parse::<usize>().ok().and_then(|a| a.checked_sub(1)).and_then(|a| u8::try_from(a).ok());
                    match a {
                        Some(a) if m.angles.contains(&a) => klipp_core::MulticamSound::Angle(a),
                        _ => return Err(format!("sound is an angle the group has ({have:?}), \"follow\" or \"off\"; not {n:?}")),
                    }
                }
            };
            drop(seq);
            send(AppCommand::SetMulticamSound { sound, at: Some(at) })
        }

        ControlCommand::AddMarker { at_secs, duration_secs, label, note, color, kind } => {
            let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
            let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s.max(0.0), fps.rate());
            let marker_id = klipp_core::MarkerId::new();
            let mut v = send(AppCommand::AddMarkerWith {
                marker_id,
                position: tc(at_secs),
                duration: duration_secs.filter(|d| *d > 0.0).map(tc),
                label: label.unwrap_or_default(),
                comment: note.unwrap_or_default(),
                color: color.as_deref().map(marker_color).transpose()?.unwrap_or(klipp_core::MarkerColor::Blue),
                kind: kind.as_deref().map(marker_kind).transpose()?.unwrap_or_default(),
            })?;
            v["marker_id"] = json!(marker_id.0.to_string());
            Ok(v)
        }

        ControlCommand::ListMarkers => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            Ok(json!({ "markers": seq.markers.iter().map(|m| json!({
                "marker_id": m.id.0.to_string(),
                "at_secs": m.position.to_secs_f64(),
                "duration_secs": m.duration.map(|d| d.to_secs_f64()),
                "label": m.label,
                "note": m.comment,
                "color": format!("{:?}", m.color).to_lowercase(),
                "kind": marker_kind_name(m.kind),
            })).collect::<Vec<_>>() }))
        }

        ControlCommand::UpdateMarker { marker_id, label, note, color, kind } => {
            let id = find_marker(ctx, &marker_id)?;
            let mut commands = Vec::new();
            if let Some(label) = label {
                commands.push(AppCommand::UpdateMarkerLabel { marker_id: id, label });
            }
            if let Some(comment) = note {
                commands.push(AppCommand::UpdateMarkerComment { marker_id: id, comment });
            }
            if let Some(c) = color {
                commands.push(AppCommand::UpdateMarkerColor { marker_id: id, color: marker_color(&c)? });
            }
            if let Some(k) = kind {
                commands.push(AppCommand::UpdateMarkerKind { marker_id: id, kind: marker_kind(&k)? });
            }
            if commands.is_empty() {
                return Err("nothing to change: give label, note, color or kind".into());
            }
            let mut last = json!({});
            for c in commands {
                last = send(c)?;
            }
            Ok(last)
        }

        ControlCommand::RemoveMarker { marker_id } => {
            let id = find_marker(ctx, &marker_id)?;
            send(AppCommand::RemoveMarker { marker_id: id })
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// The media files directly in `dir`, by name: its subfolders and any
/// other file (a list, a sidecar) left out.
fn media_in_folder(dir: &std::path::Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("can't read the folder {}: {e}", dir.display()))?;
    let mut files: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && klipp_core::media_extensions::path_is_media(p))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    files.sort();
    Ok(files)
}

/// What a media item is: a sound, a still or footage (#1892).
fn media_kind(m: &klipp_media::MediaItem) -> &'static str {
    if m.is_still {
        "still"
    } else if m.video_codec.is_empty() || m.resolution.width == 0 || m.resolution.height == 0 {
        "audio"
    } else {
        "video"
    }
}

/// One item of `list_media`. A sound says so, with its rate and channels
/// and no picture size; its cover art, `artwork`, is apart from it, so
/// neither a default nor an album cover reads as footage (#1892).
fn media_listing(m: &klipp_media::MediaItem, artwork: Option<(u32, u32)>) -> serde_json::Value {
    let kind = media_kind(m);
    let picture = kind != "audio";
    json!({
        "media_id": m.id.0.to_string(),
        "filename": m.filename,
        "path": m.path.to_string_lossy(),
        "kind": kind,
        "duration_secs": m.duration.to_secs_f64(),
        "fps": if picture { Some(m.fps.as_f64()) } else { None },
        "width": if picture { Some(m.resolution.width) } else { None },
        "height": if picture { Some(m.resolution.height) } else { None },
        "video_codec": m.video_codec,
        "has_audio": m.has_audio,
        "sample_rate": m.sample_rate,
        "audio_channels": m.audio_channels,
        "artwork": artwork.map(|(width, height)| json!({ "width": width, "height": height })),
        "log_format": log_format_json(m),
        // Part of #1224: a profile 5 picture is shown with wrong colours.
        "dolby_vision_profile": m.dolby_vision_profile,
        "warning": klipp_media::dolby_vision::warning(m.dolby_vision_profile),
        "offline": m.offline,
        "tags": m.tags,
        // A library download's file, as its credit records it: preview,
        // original or the library's download (#1894).
        "download": klipp_audio_search::read_credit(&m.path).and_then(|c| c.quality),
    })
}

#[cfg(test)]
mod listing_tests {
    use super::*;

    /// #1892: a sound is listed as one, with no picture size, its rate and
    /// channels, and its cover art apart; footage keeps its size.
    #[test]
    fn a_sound_is_listed_as_sound_and_its_art_apart() {
        let mut song = klipp_media::MediaItem::synthetic_for_test("/music/swansong.mp3".into(), None);
        song.video_codec.clear();
        song.resolution = klipp_core::Resolution { width: 0, height: 0 };
        song.sample_rate = Some(44100);
        song.audio_channels = Some(2);
        let v = media_listing(&song, Some((400, 400)));
        assert_eq!(v["kind"], "audio");
        assert!(v["width"].is_null() && v["height"].is_null() && v["fps"].is_null(), "{v}");
        assert_eq!((v["sample_rate"].as_u64(), v["audio_channels"].as_u64()), (Some(44100), Some(2)));
        assert_eq!((v["artwork"]["width"].as_u64(), v["artwork"]["height"].as_u64()), (Some(400), Some(400)));

        let shot = klipp_media::MediaItem::synthetic_for_test("/footage/a.mov".into(), None);
        let v = media_listing(&shot, None);
        assert_eq!(v["kind"], "video");
        assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(shot.resolution.width as u64), Some(shot.resolution.height as u64)));
        assert!(v["artwork"].is_null());
    }

    /// Part of #1224: a Dolby Vision profile 5 item is listed with its
    /// profile and the warning that its colours are wrong; profile 8 and
    /// a file without one are listed with no warning.
    #[test]
    fn a_dolby_vision_profile_5_item_is_listed_with_its_warning() {
        let mut shot = klipp_media::MediaItem::synthetic_for_test("/footage/dv.mp4".into(), None);
        shot.dolby_vision_profile = Some(5);
        let v = media_listing(&shot, None);
        assert_eq!(v["dolby_vision_profile"].as_u64(), Some(5));
        assert_eq!(v["warning"], klipp_media::dolby_vision::UNSUPPORTED_WARNING);
        for profile in [Some(8), None] {
            shot.dolby_vision_profile = profile;
            let v = media_listing(&shot, None);
            assert_eq!(v["dolby_vision_profile"].as_u64(), profile.map(u64::from));
            assert!(v["warning"].is_null(), "{v}");
        }
    }
}
