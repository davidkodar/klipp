//! Media requests: the timeline view, the media pool, importing and relinking, multicam, and markers (#826).

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

        ControlCommand::RelinkMedia { media_id, path, force } => {
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
                "items": pool.all().map(|m| json!({
                    "media_id": m.id.0.to_string(),
                    "filename": m.filename,
                    "path": m.path.to_string_lossy(),
                    "duration_secs": m.duration.to_secs_f64(),
                    "fps": m.fps.as_f64(),
                    "width": m.resolution.width,
                    "height": m.resolution.height,
                    "video_codec": m.video_codec,
                    "has_audio": m.has_audio,
                    "log_format": log_format_json(m),
                    "offline": m.offline,
                    "tags": m.tags,
                })).collect::<Vec<_>>(),
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
