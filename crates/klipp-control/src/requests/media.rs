//! Media requests: the timeline view, the media pool, importing and relinking, multicam, and markers (#826).

use crate::poison::Recover;
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
        let pool = ctx.media_pool.read().recovered();
        pool.all().filter(|m| m.offline).map(|m| (m.id, m.filename.clone())).collect()
    };
    if offline.is_empty() {
        return None;
    }
    let used = {
        let seq = ctx.sequence.read().ok()?;
        let others = ctx.others.read().recovered();
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
            let pool = ctx.media_pool.read().recovered();
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
                let pool = ctx.media_pool.read().recovered();
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
                let pool = ctx.media_pool.read().recovered();
                klipp_media::relink::Wanted::from_item(pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?)
            };
            // Only media, as opening a project takes it: force is for a clip
            // that looks like another, not for a file that is no clip.
            let project = ctx.shared.project.lock().recovered().path.clone();
            let trusted = klipp_media::media_check::trusted_roots(project.as_deref());
            let trusted: Vec<&std::path::Path> = trusted.iter().map(|p| p.as_path()).collect();
            if let Err(why) = klipp_media::media_check::media_file_check(&p, &trusted) {
                return Err(format!("{path} was not relinked: {why}"));
            }
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
            // Turning proxies off makes none.
            if use_proxies == Some(false) {
                let v = send_envelope(ctx, vec![AppCommand::SetUseProxies(false)], false)?;
                return Ok(json!({ "accepted": v["accepted"], "applied": v["applied"], "operation_id": v["operation_id"], "made": 0, "making": 0, "text": "Playback reads the originals. No proxies were made." }));
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
                let pool = ctx.media_pool.read().recovered();
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
                    if item.proxy_on_disk() { ready += 1 } else { wanted.push(id) }
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

        ControlCommand::VeylvikComposition { clip_id, make, open, set, version_action, version, name, reset, remove, copy_from } => {
            let id = klipp_core::ClipId(parse_uuid(&clip_id, "clip_id")?);
            let named = |ctx: &ControlContext| -> Result<(Option<klipp_timeline::CompositionId>, bool), String> {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let (t, c) = seq.find_clip(id).ok_or_else(|| format!("no clip {} in the open sequence", id.0))?;
                let clip = &seq.tracks[t].clips[c];
                Ok((clip.composition, clip.media_id().is_some()))
            };
            let (had, has_media) = named(ctx)?;
            if (make || open) && !has_media {
                return Err("the clip has no media to compose with: a video or still clip has".into());
            }
            if open || (make && had.is_none()) {
                send_envelope(ctx, vec![AppCommand::VeylvikComposition { clip_id: id, open }], false)?;
            }
            if [reset, remove, copy_from.is_some()].iter().filter(|b| **b).count() > 1 {
                return Err("reset, remove and copy_from are one at a time".into());
            }
            let comp_of = |ctx: &ControlContext, clip: klipp_core::ClipId| -> Result<Option<klipp_timeline::CompositionId>, String> {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let (t, c) = seq.find_clip(clip).ok_or_else(|| format!("no clip {} in the open sequence", clip.0))?;
                Ok(seq.tracks[t].clips[c].composition)
            };
            if let Some(from) = copy_from.as_deref() {
                use klipp_core::CompositionClipAction as A;
                let from = klipp_core::ClipId(parse_uuid(from, "copy_from")?);
                if comp_of(ctx, from)?.is_none() {
                    return Err("the clip copy_from names has no Veylvik composition".into());
                }
                if !has_media {
                    return Err("the clip has no media to compose with: a video or still clip has".into());
                }
                send_envelope(ctx, vec![AppCommand::CompositionClips { clip_ids: vec![from], action: A::Copy }, AppCommand::CompositionClips { clip_ids: vec![id], action: A::Paste }], false)?;
            }
            if reset || remove {
                use klipp_core::CompositionClipAction as A;
                if comp_of(ctx, id)?.is_none() {
                    return Err("the clip has no Veylvik composition".into());
                }
                send_envelope(ctx, vec![AppCommand::CompositionClips { clip_ids: vec![id], action: if reset { A::Reset } else { A::Remove } }], false)?;
            }
            if let Some(what) = version_action.as_deref() {
                use klipp_core::VersionAction as A;
                let place = || version.filter(|v| *v >= 1).map(|v| v - 1).ok_or("version (from 1) names the version");
                let action = match what {
                    "new" => A::New,
                    "duplicate" => A::Duplicate,
                    "switch" => A::Switch(place()?),
                    "rename" => A::Rename(place()?, name.clone().ok_or("name gives the version's new name")?),
                    "remove" => A::Delete(place()?),
                    other => return Err(format!("version_action is new, duplicate, switch, rename or remove, not {other}")),
                };
                let before = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    let (t, c) = seq.find_clip(id).ok_or("the clip is gone")?;
                    let clip = &seq.tracks[t].clips[c];
                    if clip.composition.is_none() && !matches!(action, A::New) {
                        return Err("the clip has no Veylvik composition; make: true or version_action new gives it one".into());
                    }
                    let n = klipp_timeline::veylvik::versions(clip).len();
                    if let A::Switch(i) | A::Rename(i, _) | A::Delete(i) = &action {
                        if *i >= n {
                            return Err(format!("the clip has {n} version(s)"));
                        }
                    }
                    if matches!(action, A::Delete(_)) && n < 2 {
                        return Err("a clip's last version can't be removed".into());
                    }
                    (clip.composition, clip.composition_versions.clone())
                };
                send_envelope(ctx, vec![AppCommand::CompositionVersion { clip_id: id, action }], false)?;
                let after = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    seq.find_clip(id).map(|(t, c)| (seq.tracks[t].clips[c].composition, seq.tracks[t].clips[c].composition_versions.clone()))
                };
                if after.as_ref() == Some(&before) {
                    return Err("the versions weren't changed (the app says why in its message)".into());
                }
            }
            if !set.is_empty() {
                let (has, _) = named(ctx)?;
                let comp = has.ok_or("the clip has no Veylvik composition to set; make: true gives it one")?;
                let bytes = klipp_veylvik::project_compositions().get(&comp).cloned().ok_or("the project doesn't hold the clip's composition")?;
                let set: Vec<(String, serde_json::Value)> = set.into_iter().collect();
                // Tried here first, so a refusal is said in the answer.
                klipp_veylvik::controls::set_by_label(&bytes, &set)?;
                send_envelope(ctx, vec![AppCommand::SetCompositionSettings { clip_id: id, set }], false)?;
            }
            let (now, _) = named(ctx)?;
            let mut reply = veylvik_composition_reply(id, now, had.is_none() && now.is_some(), open);
            if let Ok(seq) = ctx.sequence.read() {
                if let Some((t, c)) = seq.find_clip(id) {
                    let clip = &seq.tracks[t].clips[c];
                    reply["versions"] = json!(klipp_timeline::veylvik::versions(clip).iter().enumerate()
                        .map(|(i, v)| json!({ "version": i + 1, "name": v.name, "composition_id": v.id.to_string(), "shown": Some(v.id) == clip.composition }))
                        .collect::<Vec<_>>());
                }
            }
            Ok(reply)
        }

        ControlCommand::TagMedia { media_id, add, remove } => {
            let id = klipp_core::MediaId(parse_uuid(&media_id, "media_id")?);
            let tags = {
                let pool = ctx.media_pool.read().recovered();
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
            let pool = ctx.media_pool.read().recovered();
            Ok(json!({
                "items": pool.all().map(|m| {
                    let artwork = (media_kind(m) == "audio" && !m.offline).then(|| klipp_media::artwork_size(&m.path)).flatten();
                    media_listing(m, artwork)
                }).collect::<Vec<_>>(),
            }))
        }

        ControlCommand::ImportMedia { path, paths, folder, recursive, bins, analyze } => {
            // A folder given as `path` is imported as `folder` is (#2562):
            // as one file it was refused, "Is a directory".
            let (path, folder) = if paths.is_empty() && folder.is_none() && std::path::Path::new(&path).is_dir() { (String::new(), Some(path)) } else { (path, folder) };
            // Many files: each imported as one would be, each answered on
            // its own, and a file that can't be doesn't stop the rest
            // (#1587: 42 shots took a 42-entry batch).
            if !paths.is_empty() || folder.is_some() {
                // Each file and the subfolder it came from.
                let mut files: Vec<(String, Option<String>)> = (!path.is_empty()).then_some(path).into_iter().chain(paths).map(|f| (f, None)).collect();
                // A folder walked within its limits (#3096): links not
                // followed, hidden items left out.
                let walked = match &folder {
                    Some(dir) => {
                        let found = klipp_media::folder_walk::media_in_folder(std::path::Path::new(dir), recursive).map_err(|e| format!("can't read the folder {dir}: {e}"))?;
                        files.extend(found.files.iter().map(|f| (f.path.to_string_lossy().into_owned(), f.folder.clone())));
                        Some(found)
                    }
                    None => None,
                };
                let left_out = walked.as_ref().map(|w| w.left_out()).unwrap_or_default();
                if files.is_empty() {
                    let why = if left_out.is_empty() { String::new() } else { format!(" ({})", left_out.join("; ")) };
                    return Err(format!("no media file to import: the folder holds none{why}"));
                }
                let mut results = Vec::with_capacity(files.len());
                let mut failed = 0;
                // Files new to the pool by the bin they go in, in the
                // order the bins were first met.
                let mut to_bins: Vec<(String, Vec<klipp_core::MediaId>)> = Vec::new();
                let mut not_binned = 0;
                for (file, sub) in &files {
                    match handle(ctx, ControlCommand::import_one(file.clone(), analyze), &mut None) {
                        Ok(v) => {
                            let already = v["already_imported"] == true;
                            if let (true, Some(bin), false) = (bins, sub, already) {
                                match v["media_id"].as_str().and_then(|id| id.parse().ok()) {
                                    Some(uuid) => match to_bins.iter_mut().find(|(b, _)| b == bin) {
                                        Some((_, ids)) => ids.push(klipp_core::MediaId(uuid)),
                                        None => to_bins.push((bin.clone(), vec![klipp_core::MediaId(uuid)])),
                                    },
                                    None => not_binned += 1,
                                }
                            }
                            results.push(json!({ "path": file, "folder": sub, "media_id": v["media_id"], "already_imported": already, "job_id": v["job_id"] }));
                        }
                        Err(why) => {
                            failed += 1;
                            results.push(json!({ "path": file, "folder": sub, "error": why }));
                        }
                    }
                }
                if !to_bins.is_empty() {
                    let moves = to_bins.iter().map(|(bin, ids)| AppCommand::MoveItemsToBin { media_ids: ids.clone(), bin: Some(bin.clone()) }).collect();
                    send_envelope(ctx, moves, true)?;
                }
                let done = files.len() - failed;
                let mut text = if failed == 0 { format!("{done} file{} imported", if done == 1 { "" } else { "s" }) } else { format!("{done} of {} files imported; {failed} could not be", files.len()) };
                // Where they came from, folder by folder.
                let by_folder = walked.as_ref().map(|w| w.by_folder()).unwrap_or_default();
                if by_folder.len() > 1 || by_folder.first().is_some_and(|(f, _)| f.is_some()) {
                    let each: Vec<String> = by_folder.iter().map(|(f, n)| format!("{} {n}", f.as_deref().unwrap_or("the folder itself"))).collect();
                    text.push_str(&format!(", from {} folders: {}", by_folder.len(), each.join(", ")));
                }
                if !to_bins.is_empty() {
                    let names: Vec<&str> = to_bins.iter().map(|(b, _)| b.as_str()).collect();
                    text.push_str(&format!(". In bins: {}", names.join(", ")));
                }
                if not_binned > 0 {
                    text.push_str(&format!(". {not_binned} still probing, so not in a bin yet"));
                }
                if !left_out.is_empty() {
                    text.push_str(&format!(". Left out: {}", left_out.join("; ")));
                }
                text.push('.');
                return Ok(json!({
                    "text": text,
                    "imported": done,
                    "failed": failed,
                    "results": results,
                    "folders": by_folder.iter().map(|(f, n)| json!({ "folder": f, "files": n })).collect::<Vec<_>>(),
                    "bins": to_bins.iter().map(|(b, ids)| json!({ "bin": b, "files": ids.len() })).collect::<Vec<_>>(),
                    "left_out": left_out,
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
            let pool = ctx.media_pool.read().recovered();
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
            // Only media, as opening a project takes it: ffmpeg reads
            // a text file as a picture, and CREDITS.txt came in as a clip.
            let project = ctx.shared.project.lock().recovered().path.clone();
            let trusted = klipp_media::media_check::trusted_roots(project.as_deref());
            let trusted: Vec<&std::path::Path> = trusted.iter().map(|p| p.as_path()).collect();
            if let Err(why) = klipp_media::media_check::media_file_check(&p, &trusted) {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(path.clone());
                return Err(format!("{name} was not imported: {why}"));
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
                let pool = ctx.media_pool.read().recovered();
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

        ControlCommand::PreviewTakes { clip_id, take, all, stop } => {
            if in_batch {
                return Err("preview_takes is not part of a batch".into());
            }
            if stop {
                send(AppCommand::EndTakePreview)?;
                return Ok(json!({ "text": "the take preview is ended: the timeline is the document's again" }));
            }
            let clip_id = clip_id.ok_or("preview_takes needs clip_id, or stop: true")?;
            let id = klipp_core::ClipId(parse_uuid(&clip_id, "clip_id")?);
            let seq = ctx.sequence.document();
            let slot = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).ok_or("no clip with that clip_id on the timeline")?;
            let a = slot.audition.as_deref().ok_or("not an audition: the alternatives op's create makes one")?;
            let from = match take.as_deref() {
                Some(key) => Some(a.find(key).ok_or_else(|| format!("no take {key:?}: its takes are {}", a.takes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")))?.id),
                None => None,
            };
            let first = from.and_then(|f| a.takes.iter().find(|t| t.id == f)).or_else(|| a.active_take()).map(|t| t.name.clone()).unwrap_or_default();
            send(AppCommand::PreviewTakes { clip_id: id, take: from, all })?;
            Ok(json!({
                "text": if all { format!("playing each of the {} takes in the slot in turn, from {first}; anything but a read ends it", a.takes.len()) } else { format!("playing {first} in the slot, round and round; anything but a read ends it") },
                "takes": a.takes.len(),
            }))
        }

        ControlCommand::Alternatives { clip_id } => {
            let id = klipp_core::ClipId(parse_uuid(&clip_id, "clip_id")?);
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().recovered();
            let slot = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).ok_or("no clip with that clip_id on the timeline")?;
            let Some(a) = slot.audition.as_deref() else {
                return Ok(json!({ "clip_id": clip_id, "takes": [], "note": "not an audition: the alternatives op's create makes one" }));
            };
            let takes: Vec<serde_json::Value> = a.takes.iter().map(|t| {
                let c = t.clip.as_deref().unwrap_or(slot);
                json!({
                    "take": t.id.to_string(),
                    "name": t.name,
                    "active": t.id == a.active,
                    "secs": (c.timeline_out - c.timeline_in).to_secs_f64(),
                    "shows": take_shows(c, &pool),
                    "from_secs": c.source_in.to_secs_f64(),
                    "to_secs": c.source_out.to_secs_f64(),
                    "sound": t.clip.is_none() && slot.linked_clip_id.is_some() || !t.sound.is_empty(),
                    "differs": if t.clip.is_some() { take_differs(c, slot) } else { Vec::new() },
                })
            }).collect();
            Ok(json!({ "clip_id": clip_id, "active": a.active.to_string(), "takes": takes }))
        }

        ControlCommand::ListMarkers => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().recovered();
            let mut markers: Vec<serde_json::Value> = seq.markers.iter().map(|m| json!({
                "scope": "sequence",
                "marker_id": m.id.0.to_string(),
                "at_secs": m.position.to_secs_f64(),
                "duration_secs": m.duration.map(|d| d.to_secs_f64()),
                "label": m.label,
                "note": m.comment,
                "color": format!("{:?}", m.color).to_lowercase(),
                "kind": marker_kind_name(m.kind),
            })).collect();
            // The clips' own markers too, as a timeline or list import
            // brings them: the import counted them, and a list without
            // them read as though they were lost. They ride with their
            // clip, so they have no marker_id to update or remove by.
            for track in &seq.tracks {
                for c in &track.clips {
                    let media_name = match c.kind {
                        klipp_timeline::ClipKind::Media { media_id } => pool.get(&media_id).map(|m| m.filename.as_str()),
                        _ => None,
                    };
                    for m in &c.clip_markers {
                        markers.push(json!({
                            "scope": "clip",
                            "clip_id": c.id.0.to_string(),
                            "clip": c.label(media_name.unwrap_or("")),
                            "track": track.name,
                            "at_secs": c.timeline_in.to_secs_f64() + m.time_secs,
                            "label": m.name,
                            "note": m.comment,
                            "color": format!("{:?}", m.color).to_lowercase(),
                        }));
                    }
                }
            }
            markers.sort_by(|a, b| a["at_secs"].as_f64().partial_cmp(&b["at_secs"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
            Ok(json!({ "markers": markers }))
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
        "proxy_ready": m.proxy_on_disk(),
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

    /// Each file says whether it has a proxy to read now: not when none was
    /// made, nor when the one made has gone.
    #[test]
    fn an_item_is_listed_with_whether_its_proxy_is_there() {
        let proxy = std::env::temp_dir().join(format!("klipp-listed-proxy-{}.mp4", uuid::Uuid::new_v4()));
        let none = klipp_media::MediaItem::synthetic_for_test("/footage/a.mp4".into(), None);
        assert_eq!(media_listing(&none, None)["proxy_ready"], false);
        let with = klipp_media::MediaItem::synthetic_for_test("/footage/a.mp4".into(), proxy.to_str());
        assert_eq!(media_listing(&with, None)["proxy_ready"], false, "its file isn't there");
        std::fs::write(&proxy, b"a proxy").unwrap();
        assert_eq!(media_listing(&with, None)["proxy_ready"], true);
        std::fs::remove_file(proxy).unwrap();
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

/// What a take's clip shows (#3209): its file's name, its title's first
/// line, or what kind of clip it is.
fn take_shows(c: &klipp_timeline::Clip, pool: &klipp_media::MediaPool) -> String {
    match &c.kind {
        klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).map_or_else(|| "missing media".to_string(), |m| m.filename.clone()),
        klipp_timeline::ClipKind::Title(td) => td.text().lines().next().map_or_else(|| "a title".to_string(), str::to_string),
        klipp_timeline::ClipKind::Generator(_) => "a generator".to_string(),
        klipp_timeline::ClipKind::Sequence { .. } => "a compound clip".to_string(),
        _ => "a clip".to_string(),
    }
}

/// What differs between a take's clip and the one in the slot (#3209), by
/// name: what an assistant says when it offers the takes.
fn take_differs(c: &klipp_timeline::Clip, slot: &klipp_timeline::Clip) -> Vec<&'static str> {
    let mut out = Vec::new();
    if c.media_id() != slot.media_id() || std::mem::discriminant(&c.kind) != std::mem::discriminant(&slot.kind) {
        out.push("media");
    }
    if (c.source_in, c.source_out) != (slot.source_in, slot.source_out) {
        out.push("range");
    }
    if (c.timeline_out - c.timeline_in) != (slot.timeline_out - slot.timeline_in) {
        out.push("length");
    }
    if (c.speed - slot.speed).abs() > 1e-9 || c.speed_keyframes != slot.speed_keyframes {
        out.push("speed");
    }
    if c.picture_effects().ok() != slot.picture_effects().ok() {
        out.push("effects");
    }
    if c.grade_to_run() != slot.grade_to_run() {
        out.push("grade");
    }
    if matches!((&c.kind, &slot.kind), (klipp_timeline::ClipKind::Title(a), klipp_timeline::ClipKind::Title(b)) if serde_json::to_value(a).ok() != serde_json::to_value(b).ok()) {
        out.push("title");
    }
    if (c.volume - slot.volume).abs() > 1e-6 || (c.gain - slot.gain).abs() > 1e-6 {
        out.push("sound level");
    }
    out
}

/// What `klipp_veylvik_composition` answers for clip `clip`: its
/// composition `id`'s size, rate, length, nodes and folder, as the project
/// holds it now, or that it has none.
fn veylvik_composition_reply(clip: klipp_core::ClipId, id: Option<klipp_timeline::CompositionId>, made: bool, opened: bool) -> serde_json::Value {
    let Some(id) = id else {
        return json!({
            "clip_id": clip.0.to_string(),
            "composition": null,
            "text": "The clip has no Veylvik composition; make: true gives it one made of its media, and open: true shows it on the Veylvik page.",
        });
    };
    let bytes = klipp_veylvik::project_compositions().get(&id).cloned();
    let parsed = bytes.as_deref().map(serde_json::from_slice::<klipp_veylvik::Composition>);
    let mut reply = json!({
        "clip_id": clip.0.to_string(),
        "composition_id": id.to_string(),
        "made": made,
        "opened": opened,
        "folder": klipp_veylvik::project_assets(id).map(|p| p.display().to_string()),
    });
    match parsed {
        Some(Ok(c)) => {
            // A composition's rate is frames a second, as a fraction.
            let fps = (c.frame_rate.seconds().unwrap_or(0.0) * 1000.0).round() / 1000.0;
            let nodes: Vec<serde_json::Value> = c.nodes.values().map(|n| {
                // The node's kind, as its file names it.
                let kind = serde_json::to_value(&n.operation).ok().and_then(|v| v.as_object().and_then(|o| o.keys().next().cloned()).or_else(|| v.as_str().map(str::to_owned))).unwrap_or_default();
                json!({ "id": n.id.to_string(), "label": n.label, "kind": kind, "output": n.id == c.output })
            }).collect();
            reply["width"] = json!(c.width);
            reply["height"] = json!(c.height);
            reply["fps"] = json!(fps);
            reply["frames"] = json!(c.frame_count);
            reply["text"] = json!(format!(
                "{} the clip's Veylvik composition: {}×{} at {fps} fps, {} frames, {} node(s){}.",
                if made { "Made" } else { "Read" }, c.width, c.height, c.frame_count, nodes.len(),
                if opened { ", open on the Veylvik page" } else { "" }
            ));
            reply["nodes"] = json!(nodes);
            let shown: Vec<serde_json::Value> = bytes.as_deref().and_then(|b| klipp_veylvik::controls::published(b).ok()).unwrap_or_default().into_iter()
                .map(|p| json!({ "label": p.label, "node": p.node.to_string(), "parameter": p.parameter, "value": p.value }))
                .collect();
            reply["settings"] = json!(shown);
        }
        Some(Err(e)) => reply["text"] = json!(format!("The clip names composition {id}, which can't be read: {e}; it shows its media.")),
        None => reply["text"] = json!(format!("The clip names composition {id}, which the project doesn't hold; it shows its media.")),
    }
    reply
}

#[cfg(test)]
mod veylvik_composition_tests {
    use super::veylvik_composition_reply;

    /// A clip without a composition is told how to make one; with one,
    /// its size, rate, length and nodes are read from what the project
    /// holds, the output node named.
    #[test]
    fn a_clips_composition_is_read_as_the_project_holds_it() {
        let clip = klipp_core::ClipId::new();
        let none = veylvik_composition_reply(clip, None, false, false);
        assert!(none["composition"].is_null());
        assert!(none["text"].as_str().unwrap().contains("make: true"));

        let id = klipp_timeline::CompositionId::new();
        let c = klipp_veylvik::Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [1920, 1080], klipp_veylvik::Time { ticks: 25, rate: 1 }, 50).unwrap();
        klipp_veylvik::set_project_compositions([(id, std::sync::Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
        let read = veylvik_composition_reply(clip, Some(id), true, false);
        klipp_veylvik::set_project_compositions(Default::default());
        assert_eq!((read["width"].as_u64(), read["height"].as_u64(), read["frames"].as_u64()), (Some(1920), Some(1080), Some(50)));
        assert_eq!(read["fps"].as_f64(), Some(25.0));
        assert_eq!(read["made"], true);
        let nodes = read["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), c.nodes.len());
        assert_eq!(nodes.iter().filter(|n| n["output"] == true).count(), 1);
        assert!(nodes.iter().all(|n| !n["kind"].as_str().unwrap().is_empty()), "{nodes:?}");
        assert!(read["text"].as_str().unwrap().starts_with("Made the clip's Veylvik composition: 1920×1080"), "{}", read["text"]);
    }
}

