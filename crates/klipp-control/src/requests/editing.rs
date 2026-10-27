//! Editing requests: projects and sequences, clips appended, removed and selected, surfaces, plans and their application, fonts (#826).

#[allow(unused_imports)]
use crate::*;

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod normalize_tests;

/// The catalogue's families as the fonts reply lists them (#2732): name,
/// tags, size and whether fetched.
fn catalogue_json(families: &[&klipp_title::catalogue::CatalogueFamily]) -> Vec<serde_json::Value> {
    families
        .iter()
        .map(|f| json!({ "name": f.name, "style": f.style, "mood": f.mood, "scripts": f.scripts, "licence": f.licence, "mb": (f.files.iter().map(|x| x.size).sum::<u64>() as f64 / 1e5).round() / 10.0, "fetched": klipp_title::catalogue::is_fetched(f) }))
        .collect()
}

/// The fonts reply (#2492): each family once, in the JSON; the text
/// names the bundled ones and how many are installed, or with `query`
/// every family whose name holds it, either way. Klipp's catalogue
/// (#2732) is listed too, by name or by what a family is (a query of
/// tags: "condensed news sans", "japanese", "wedding script").
pub(crate) fn fonts_reply(bundled: Vec<String>, installed: Vec<String>, query: Option<&str>) -> serde_json::Value {
    let fallback = klipp_title::bundled::DEFAULT_FAMILY;
    let Some(q) = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty()) else {
        let catalogue: Vec<_> = klipp_title::catalogue::families().iter().collect();
        let names: Vec<&str> = catalogue.iter().map(|f| f.name.as_str()).collect();
        let text = format!(
            "Bundled with Klipp, always there: {}.\n{} families installed on this machine: in installed, or ask with query for those whose name holds a word.\nIn Klipp's catalogue, open-licence families fetched on request (fetch: name): {}. A query of what a family is finds them: condensed news sans, engraved serif, rounded, script, mono, japanese, arabic.\nA title or type style takes any of these by its family name; anything else falls back to {fallback}.",
            bundled.join(", "),
            installed.len(),
            names.join(", "),
        );
        return json!({ "text": text, "bundled": bundled, "installed": installed, "catalogue": catalogue_json(&catalogue), "fallback": fallback });
    };
    let hit = |names: Vec<String>| -> Vec<String> { names.into_iter().filter(|n| n.to_lowercase().contains(&q)).collect() };
    let (bundled, installed) = (hit(bundled), hit(installed));
    let catalogue = klipp_title::catalogue::query(&q);
    let names: Vec<&str> = catalogue.iter().map(|f| f.name.as_str()).collect();
    let text = match (bundled.is_empty(), installed.is_empty(), catalogue.is_empty()) {
        (true, true, true) => format!("No family's name holds \"{q}\", and none in the catalogue is that; anything a title names that isn't here falls back to {fallback}."),
        _ => format!(
            "Families for \"{q}\": bundled {}; installed {}; in the catalogue, fetched on request {}.",
            if bundled.is_empty() { "none".into() } else { bundled.join(", ") },
            if installed.is_empty() { "none".into() } else { installed.join(", ") },
            if names.is_empty() { "none".into() } else { names.join(", ") },
        ),
    };
    json!({ "text": text, "bundled": bundled, "installed": installed, "catalogue": catalogue_json(&catalogue), "fallback": fallback, "query": q })
}

/// Fetch the catalogue family `name` (#2732): checked by its hashes, kept
/// with its licence in the user's data folder; a title names it then as
/// any font. Refused for a name the catalogue has not, with the nearest.
fn fetch_font(name: &str) -> Result<serde_json::Value, String> {
    let Some(family) = klipp_title::catalogue::family(name) else {
        let near: Vec<&str> = klipp_title::catalogue::query(name).iter().map(|f| f.name.as_str()).collect();
        return Err(format!(
            "\"{name}\" is not in Klipp's font catalogue{}; klipp_fonts lists it, and a query of what you want (condensed news sans, script) finds families",
            if near.is_empty() { String::new() } else { format!(" (by that: {})", near.join(", ")) }
        ));
    };
    let was = klipp_title::catalogue::is_fetched(family);
    let dir = klipp_title::catalogue::fetch(family)?;
    let text = format!(
        "{} {} ({} licence, kept beside it in {}). Set a title in it by its name: font \"{}\".",
        family.name,
        if was { "was already fetched" } else { "is fetched" },
        family.licence,
        dir.display(),
        family.name,
    );
    Ok(json!({ "text": text, "family": family.name, "fetched": true, "path": dir, "licence": family.licence }))
}

#[cfg(test)]
mod fonts_reply_tests {
    use super::fonts_reply;

    /// #2492: each family once, the installed ones in the JSON alone, and
    /// a query narrows both lists.
    #[test]
    fn fonts_are_listed_once_and_found_by_a_word() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let all = fonts_reply(names(&["Inter", "Playfair Display"]), names(&["DejaVu Sans", "Noto Serif Display"]), None);
        let text = all["text"].as_str().unwrap();
        assert!(text.contains("Inter, Playfair Display") && text.contains("2 families installed") && !text.contains("DejaVu"), "{text}");
        assert_eq!(all["installed"].as_array().unwrap().len(), 2);
        let found = fonts_reply(names(&["Inter", "Playfair Display"]), names(&["DejaVu Sans", "Noto Serif Display"]), Some("display"));
        assert_eq!((found["bundled"].as_array().unwrap().len(), found["installed"].as_array().unwrap().len()), (1, 1), "{found}");
        assert!(found["text"].as_str().unwrap().contains("Noto Serif Display"));
        let none = fonts_reply(names(&["Inter"]), names(&[]), Some("gothic"));
        assert!(none["text"].as_str().unwrap().starts_with("No family's name holds"));
    }

    /// #2732: the catalogue is listed, found by what a family is, its
    /// fetch named where a plan asks for one not fetched yet, and a name
    /// it has not refused with the nearest.
    #[test]
    fn the_catalogue_is_listed_found_and_fetched_by_name() {
        let all = fonts_reply(vec!["Inter".into()], vec![], None);
        assert!(all["catalogue"].as_array().unwrap().len() >= 30, "{all}");
        assert!(all["text"].as_str().unwrap().contains("Cinzel"));
        let news = fonts_reply(vec![], vec![], Some("condensed news sans"));
        let found: Vec<&str> = news["catalogue"].as_array().unwrap().iter().filter_map(|f| f["name"].as_str()).collect();
        assert!(found.contains(&"Oswald") && found.contains(&"Roboto Condensed"), "{found:?}");
        assert!(news["text"].as_str().unwrap().contains("Oswald"));
        if !klipp_title::catalogue::fetched("Great Vibes") {
            let w = crate::plans::font_warnings(["Great Vibes"].into_iter());
            assert!(w[0].contains("catalogue") && w[0].contains("fetch: \"Great Vibes\""), "{w:?}");
        }
        let w = crate::plans::font_warnings(["No Such Face"].into_iter());
        assert!(w[0].contains("neither bundled nor installed"), "{w:?}");
        let err = super::fetch_font("wedding script").unwrap_err();
        assert!(err.contains("not in Klipp's font catalogue") && err.contains("Great Vibes"), "{err}");
    }
}

/// Where the user's project templates are kept (#618), as the app keeps
/// them.
fn templates_dir() -> std::path::PathBuf {
    klipp_project::template::templates_dir(&dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
}

/// The template called `name` in `dir` (any case), or why not: the names
/// there are listed.
fn template_named(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    let listed = klipp_project::template::list_templates(dir);
    match listed.iter().find(|(_, t)| t.as_ref().is_ok_and(|t| t.name.eq_ignore_ascii_case(name.trim()))) {
        Some((path, _)) => Ok(path.clone()),
        None => {
            let names: Vec<String> = listed.into_iter().filter_map(|(_, t)| t.ok().map(|t| t.name)).collect();
            Err(if names.is_empty() { format!("no project template \"{name}\": there are none yet") } else { format!("no project template \"{name}\"; there are {}", names.join(", ")) })
        }
    }
}

/// A template's need as JSON: its kind, what, and whether it is missing.
fn dependency_json(d: &klipp_project::template::Dependency) -> serde_json::Value {
    use klipp_project::template::Dependency as D;
    match d {
        D::PackagedMedia(name) => json!({ "media": name, "copied": true }),
        D::LinkedMedia { path, present } => json!({ "media": path, "copied": false, "missing": !present }),
        D::Lut { path, present } => json!({ "lut": path, "missing": !present }),
        D::Font(f) => json!({ "font": f }),
        D::Plugin(p) => json!({ "plugin": p }),
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
        ControlCommand::NewProject { name, location, width, height, fps, sequence_name, discard_changes, template } => {
            if in_batch {
                return Err("new_project is not part of a batch; send it on its own".into());
            }
            let name = name.trim().to_string();
            if name.is_empty() || name.contains('/') {
                return Err("a project needs a name, without slashes".into());
            }
            let location = std::path::PathBuf::from(&location);
            if !location.is_absolute() || !location.is_dir() {
                return Err(format!("location must be an existing folder, as an absolute path (got {location:?})"));
            }
            let dir = location.join(format!("{name}.klipp"));
            if dir.exists() {
                return Err(format!("{} exists already: open_project it, or pick another name", dir.display()));
            }
            let (fps_num, fps_den) = match fps {
                Some(f) => fps_fraction(f)?,
                None => (0, 0),
            };
            let (width, height) = match (width, height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
                (None, None) => (0, 0),
                _ => return Err("give both width and height, or neither".into()),
            };
            let before = project_status(ctx)?;
            if before.dirty && !discard_changes {
                return Err("the open project has unsaved changes: save_project first, or pass discard_changes true to lose them".into());
            }
            let sequence_name = sequence_name.unwrap_or_else(|| "Main Sequence".into());
            // A template by name (#618), found before anything is replaced.
            let template = match template {
                Some(t) => Some(template_named(&templates_dir(), &t)?),
                None => None,
            };
            let from_template = template.is_some();
            send_envelope(ctx, vec![AppCommand::CreateProject { name, location, fps_num, fps_den, width, height, sequence_name, template, hdr: false }], false)?;
            let after = wait_for_project_event(ctx, before.generation, 60_000)?;
            if let Some(e) = after.last_error {
                return Err(e);
            }
            // The project is made before its first save has landed: answer
            // once the file is there, as save_project does, so what reads
            // the folder straight after finds it (#784).
            let file = dir.join("project.json");
            let waited = std::time::Instant::now();
            while !file.exists() && waited.elapsed() < std::time::Duration::from_secs(10) {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let saved = file.exists();
            let made = if from_template {
                "Project made from the template."
            } else if width == 0 {
                "Empty project made; its size and rate follow the first clip placed."
            } else {
                "Empty project made."
            };
            Ok(json!({
                "created": true,
                "saved": saved,
                "path": dir.to_string_lossy(),
                "text": if saved { made.to_string() } else { format!("{made} Its first save has not landed yet: wait before reading the folder.") },
            }))
        }

        ControlCommand::ProjectTemplates { save } => {
            if in_batch {
                return Err("project_templates is not part of a batch; send it on its own".into());
            }
            let dir = templates_dir();
            let mut saved = None;
            if let Some(save) = save {
                let name = save.name.trim().to_string();
                if name.is_empty() {
                    return Err("a template needs a name".into());
                }
                let project = ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?;
                let mut starter = Vec::new();
                for wanted in &save.starter_sequences {
                    let Some(s) = project.sequences.iter().find(|s| s.name.eq_ignore_ascii_case(wanted.trim())) else {
                        let names: Vec<&str> = project.sequences.iter().map(|s| s.name.as_str()).collect();
                        return Err(format!("no sequence \"{wanted}\"; the project's are {}", names.join(", ")));
                    };
                    starter.push(klipp_core::SequenceId(parse_uuid(&s.id, "sequence id")?));
                }
                drop(project);
                let stamp = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
                let before: Vec<(std::path::PathBuf, Option<std::time::SystemTime>)> = klipp_project::template::list_templates(&dir).into_iter().map(|(p, _)| { let t = stamp(&p); (p, t) }).collect();
                send_envelope(ctx, vec![AppCommand::SaveProjectTemplate { name: name.clone(), description: save.description.trim().to_string(), starter, package_media: save.package_media }], false)?;
                // Kept once its file is there, new or written anew.
                let waited = std::time::Instant::now();
                saved = loop {
                    let found = klipp_project::template::list_templates(&dir).into_iter().find(|(p, t)| {
                        t.as_ref().is_ok_and(|t| t.name == name) && !before.iter().any(|(q, at)| q == p && *at == stamp(p))
                    });
                    if let Some((path, _)) = found {
                        break Some(path);
                    }
                    if waited.elapsed() > std::time::Duration::from_secs(60) {
                        return Err(format!("the template \"{name}\" was not saved: see the app's message"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                };
            }
            let templates: Vec<serde_json::Value> = klipp_project::template::list_templates(&dir)
                .into_iter()
                .map(|(path, t)| match t {
                    Ok(t) => {
                        let needs: Vec<serde_json::Value> = t.dependencies(&path).iter().map(dependency_json).collect();
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "holds": t.summary(),
                            "starts_with": t.starter.as_ref().map(|s| s.sequences.clone()).unwrap_or_default(),
                            "needs": needs,
                        })
                    }
                    Err(e) => json!({ "file": path.file_name().map(|n| n.to_string_lossy().into_owned()), "unreadable": e }),
                })
                .collect();
            let text = match &saved {
                Some(p) => format!("Saved the template at {}; {} template(s) now.", p.display(), templates.len()),
                None if templates.is_empty() => "No project templates yet: save one with save {name}.".into(),
                None => format!("{} project template(s): start a project from one with new_project's template.", templates.len()),
            };
            Ok(json!({ "templates": templates, "text": text }))
        }

        ControlCommand::NewSequence { name, width, height, fps } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err("a sequence needs a name".into());
            }
            if width == 0 || height == 0 || width % 2 == 1 || height % 2 == 1 {
                return Err(format!("{width}x{height} is not a picture size: both even and above zero"));
            }
            let (fps_num, fps_den) = fps_fraction(fps)?;
            send(AppCommand::AddSequenceAndOpen { name, fps_num, fps_den, width, height })
        }

        ControlCommand::MakeShorts { media_id, moments, platform, tighten, hook_title } => {
            if in_batch {
                return Err("make_shorts builds sequences of its own: send it on its own".into());
            }
            // Every short platform asks for about -14 LUFS (#2720).
            let lufs = match platform.as_deref().map(str::to_lowercase).as_deref() {
                None | Some("shorts" | "reels" | "tiktok") => -14.0,
                Some(other) => return Err(format!("\"{other}\" is not a platform: give shorts, reels or tiktok")),
            };
            if moments.is_empty() || moments.len() > 10 {
                return Err(format!("{} moments: give 1 to 10, from find_moments", moments.len()));
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (item, words) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?.clone();
                let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
                let Some(t) = report.transcript else {
                    return Err(format!("{} has no transcript yet: analyze it first", item.filename));
                };
                let words: Vec<klipp_document::ops::SpokenWord> = t.words.iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text.clone(), confidence: w.confidence }).collect();
                (item, words)
            };
            let dur = item.duration.to_secs_f64();
            let tighten = tighten.unwrap_or(true);
            // Each moment's pieces first, so a bad one is refused before
            // any sequence is made.
            let mut planned = Vec::new();
            for (k, m) in moments.iter().enumerate() {
                if !(m.from_secs >= 0.0 && m.to_secs > m.from_secs && m.to_secs <= dur + 0.5) {
                    return Err(format!("moment {}: {:.2}-{:.2} s is not inside the file's {dur:.2} s", k + 1, m.from_secs, m.to_secs));
                }
                let pieces = klipp_document::shorts::pieces(&words, m.from_secs, m.to_secs, tighten);
                if pieces.is_empty() {
                    return Err(format!("moment {}: nothing is said in {:.2}-{:.2} s", k + 1, m.from_secs, m.to_secs));
                }
                let hook = m.hook.clone().unwrap_or_else(|| words.iter().filter(|w| w.start_secs >= m.from_secs - 0.05).take(8).map(|w| w.text.trim()).collect::<Vec<_>>().join(" "));
                planned.push((pieces, hook));
            }
            let mut made = Vec::new();
            for (k, (pieces, hook)) in planned.iter().enumerate() {
                let short: String = hook.chars().take(40).collect();
                let name = format!("Short {}: {}", k + 1, short.trim());
                crate::handle(ctx, ControlCommand::NewSequence { name: name.clone(), width: 1080, height: 1920, fps: item.fps.as_f64() }, &mut None)?;
                let ops = klipp_document::shorts::short_ops(id, pieces, hook_title.then_some(hook.as_str()), 1.15);
                crate::handle(ctx, ControlCommand::Apply { ops, expected_revision: None, expected_project_identity: None, label: Some(format!("Make Short {}", k + 1)), detail: false }, &mut None)?;
                let sounds: Vec<String> = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).flat_map(|t| &t.clips).map(|c| c.id.0.to_string()).collect();
                if !sounds.is_empty() {
                    crate::handle(ctx, ControlCommand::NormalizeClips { clip_ids: sounds, mode: klipp_core::normalize::NormalizeMode::Loudness, target: Some(lufs), ceiling_dbtp: Some(-1.0), group: true, apply: Some(true) }, &mut None)?;
                }
                let secs: f64 = pieces.iter().map(|(a, b)| b - a).sum();
                made.push(json!({ "name": name, "pieces": pieces.len(), "secs": secs }));
            }
            let text = made.iter().map(|m| format!("  {} ({} piece(s), {:.1} s)", m["name"].as_str().unwrap_or_default(), m["pieces"], m["secs"].as_f64().unwrap_or_default())).collect::<Vec<_>>().join("\n");
            Ok(json!({
                "text": format!("{} short(s) made, each a sequence of its own at 1080x1920, {lufs} LUFS:\n{text}", made.len()),
                "shorts": made,
                "next": "Open each (klipp_switch_sequence) and look: klipp_frame_at over the cuts, the captions with klipp_timeline, then klipp_check. Pictures with no clear speaker stay centred: reframe them by hand.",
            }))
        }

        ControlCommand::RebuildVersion { sequence_id } => {
            if in_batch {
                return Err("rebuild_version is not part of a batch: send it on its own".into());
            }
            let project = ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?;
            let index = match sequence_id.as_deref() {
                Some(id) => project.sequences.iter().position(|s| s.id == id).ok_or_else(|| format!("no sequence {id}"))?,
                None => project.active_sequence,
            };
            let Some(seq) = project.sequences.get(index) else { return Err("no sequence is open".into()) };
            let Some(version) = seq.version.as_ref() else {
                return Err(format!("{} is no version of another sequence: nothing to rebuild it from", seq.name));
            };
            if !version.stale {
                return Err(format!("{} already has its master's cut: nothing to rebuild", seq.name));
            }
            drop(project);
            let mut v = send(AppCommand::RebuildVersion { sequence_idx: index })?;
            v["next"] = json!("Its clips still in the cut keep their framing; the new ones are reframed once their framing analysis is done. Look at the new ones with klipp_frame_at.");
            Ok(v)
        }

        ControlCommand::RenameSequence { name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err("a sequence needs a name".into());
            }
            send(AppCommand::RenameSequence { sequence_idx: usize::MAX, new_name: name })
        }

        ControlCommand::SwitchSequence { index, sequence_id } => {
            let project = ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?;
            let found = match (index, sequence_id.as_deref()) {
                (Some(i), None) => (i < project.sequences.len()).then_some(i),
                (None, Some(id)) => project.sequences.iter().position(|s| s.id == id),
                _ => return Err("give the sequence's index or its sequence_id, one of them".into()),
            };
            let Some(sequence_idx) = found else {
                return Err(format!("no such sequence; the project has {}", project.sequences.len()));
            };
            drop(project);
            send(AppCommand::SetActiveSequence { sequence_idx })
        }

        ControlCommand::DuplicateSequence { index, sequence_id, name, open } => {
            // Indices move as a batch's steps add and remove sequences.
            if in_batch {
                return Err("duplicate_sequence is not part of a batch; send it on its own".into());
            }
            let project = ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?;
            let found = match (index, sequence_id.as_deref()) {
                (Some(i), None) => (i < project.sequences.len()).then_some(i),
                (None, Some(id)) => project.sequences.iter().position(|s| s.id == id),
                (None, None) => (project.active_sequence < project.sequences.len()).then_some(project.active_sequence),
                _ => return Err("give the sequence's index or its sequence_id, not both".into()),
            };
            let Some(sequence_idx) = found else {
                return Err(format!("no such sequence; the project has {}", project.sequences.len()));
            };
            let name = name.map(|n| n.trim().to_string()).unwrap_or_else(|| format!("{} copy", project.sequences[sequence_idx].name));
            if name.is_empty() {
                return Err("a sequence needs a name".into());
            }
            let (from, copy_idx) = (project.sequences[sequence_idx].name.clone(), project.sequences.len());
            drop(project);
            let mut cmds = vec![AppCommand::DuplicateSequence { sequence_idx, new_name: name.clone() }];
            if open {
                cmds.push(AppCommand::SetActiveSequence { sequence_idx: copy_idx });
            }
            let mut reply = send_envelope(ctx, cmds, false)?;
            reply["index"] = json!(copy_idx);
            reply["name"] = json!(name);
            reply["text"] = json!(format!("\"{name}\" is a copy of \"{from}\", sequence {copy_idx}{}", if open { ", open now" } else { "" }));
            Ok(reply)
        }

        ControlCommand::RemoveSequence { index, sequence_id, confirm } => {
            let project = ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?;
            let found = match (index, sequence_id.as_deref()) {
                (Some(i), None) => (i < project.sequences.len()).then_some(i),
                (None, Some(id)) => project.sequences.iter().position(|s| s.id == id),
                _ => return Err("give the sequence's index or its sequence_id, one of them".into()),
            };
            let Some(sequence_idx) = found else {
                return Err(format!("no such sequence; the project has {}", project.sequences.len()));
            };
            if project.sequences.len() == 1 {
                return Err("it is the project's only sequence: add another first".into());
            }
            let seq = &project.sequences[sequence_idx];
            if let Some(parent) = &seq.used_in {
                return Err(format!("\"{}\" is a compound clip in \"{parent}\": remove that clip first", seq.name));
            }
            if !confirm {
                return Err(format!("\"{}\" holds {} clip(s) and its removal can't be undone: ask first, then send confirm true", seq.name, seq.clips));
            }
            let sequence_id = klipp_core::SequenceId(parse_uuid(&seq.id, "sequence_id")?);
            drop(project);
            send(AppCommand::DeleteSequence { sequence_id })
        }

        ControlCommand::AppendClip { media_id, track } => {
            let id = klipp_core::MediaId(parse_uuid(&media_id, "media_id")?);
            match track {
                Some(t) => send(AppCommand::AppendClipToTrack { media_id: id, track_index: t }),
                None => send(AppCommand::AppendClip { media_id: id }),
            }
        }

        ControlCommand::RemoveClip { clip_id: c } => {
            send(AppCommand::RemoveClip { clip_id: existing_clip_id(ctx, &c)? })
        }

        ControlCommand::AttachAudio { clip_id: c } => {
            send(AppCommand::AttachLinkedAudio { clip_id: existing_clip_id(ctx, &c)? })
        }

        ControlCommand::NormalizeClips { clip_ids, mode, target, ceiling_dbtp, group, apply } => {
            if in_batch {
                return Err("normalize_clips measures the clips first; send it on its own".into());
            }
            use klipp_core::normalize::{NormalizeMode, NormalizeSettings};
            let target = target.unwrap_or(match mode {
                NormalizeMode::Loudness => klipp_core::normalize::LOUDNESS_PRESETS[0].1,
                NormalizeMode::Peak => -1.0,
            });
            let settings = NormalizeSettings { mode, target, ceiling_dbtp: ceiling_dbtp.unwrap_or(klipp_core::DEFAULT_TRUE_PEAK_DBTP as f64), group };
            settings.check()?;
            if clip_ids.is_empty() {
                return Err("name the clips to normalize in clip_ids".into());
            }
            let ids = clip_ids.iter().map(|c| existing_clip_id(ctx, c)).collect::<Result<Vec<_>, _>>()?;
            let (clips, skipped) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_render::normalize::clip_sounds(&seq, &pool, &ids)
            };
            if clips.is_empty() {
                return Err("none of those clips plays sound from a file".into());
            }
            let sounds: Vec<_> = clips.iter().map(|(_, s)| s.clone()).collect();
            let measured = klipp_render::normalize::measure(&sounds, &std::sync::atomic::AtomicBool::new(false)).map_err(|e| format!("the clips could not be measured: {e}"))?;
            let gains = klipp_core::normalize::gains(&measured.levels, &measured.together, &settings);
            let round = |v: f64| if v.is_finite() { json!((v * 100.0).round() / 100.0) } else { json!(null) };
            let rows: Vec<serde_json::Value> = clips
                .iter()
                .zip(measured.levels.iter().zip(&gains))
                .map(|((id, _), (level, gain))| {
                    json!({
                        "clip_id": id.to_string(),
                        "integrated_lufs": round(level.integrated_lufs),
                        "sample_peak_dbfs": round(level.sample_peak_dbfs),
                        "true_peak_dbtp": round(level.true_peak_dbtp),
                        "gain_db": gain.map(|g| round(g.db)),
                        "ceiling_limited": gain.is_some_and(|g| g.ceiling_limited),
                        "max_limited": gain.is_some_and(|g| g.max_limited),
                    })
                })
                .collect();
            let set: Vec<(klipp_core::ClipId, f32)> = clips.iter().zip(&gains).filter_map(|((id, _), g)| g.map(|g| (*id, 10f64.powf(g.db / 20.0) as f32))).collect();
            let mut v = if apply.unwrap_or(true) && !set.is_empty() {
                send(AppCommand::ApplyClipGains { gains: set })?
            } else {
                json!({ "applied": false })
            };
            v["clips"] = json!(rows);
            v["skipped"] = json!(skipped.iter().map(|c| c.to_string()).collect::<Vec<_>>());
            v["settings"] = json!(settings);
            if settings.group {
                v["together"] = json!({ "integrated_lufs": round(measured.together.integrated_lufs), "true_peak_dbtp": round(measured.together.true_peak_dbtp) });
            }
            Ok(v)
        }

        ControlCommand::SetAppendAudio { enabled } => {
            send(AppCommand::SetAppendBringsAudio(enabled))
        }

        ControlCommand::DetectSceneCuts { clip_id } => {
            if in_batch {
                return Err("detect_scene_cuts runs in the background; send it on its own".into());
            }
            let id = existing_clip_id(ctx, &clip_id)?;
            // What the app would refuse, refused here, not answered as
            // started (#685 review).
            {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                if let Some(why) = klipp_document::ops::scene_cut_refusal(&seq, &pool, id) {
                    return Err(format!("no scene cuts to find: {why}"));
                }
            }
            let mut v = send(AppCommand::DetectSceneCuts { clip_id: id })?;
            v["text"] = json!("Finding the scene cuts in the background; the clip is split at them, its sound with it, as one undo step when it is done. Read the timeline in a while to see the shots, or undo to put it back.");
            Ok(v)
        }

        ControlCommand::TrackSurface { clip_id, corners, click, track_corners, exclude, at_secs, name, surface, direction } => {
            let direction: i8 = match direction.as_deref().map(str::trim).map(str::to_lowercase).as_deref() {
                None | Some("") | Some("both") => 0,
                Some("forward") | Some("later") => 1,
                Some("backward") | Some("earlier") => -1,
                Some(other) => return Err(format!("direction \"{other}\" is not one: use forward, backward or both")),
            };
            if direction != 0 && surface.is_none() {
                return Err("a track continues one way only on a surface it already has: give surface".into());
            }
            if in_batch {
                return Err("track_surface runs in the background; send it on its own".into());
            }
            let id = existing_clip_id(ctx, &clip_id)?;
            match (corners, click) {
                (Some(_), Some(_)) => return Err("give corners or click, not both".into()),
                (None, None) => return Err("give the surface's four corners, or click: a point inside it, fractions of the frame".into()),
                (None, Some(c)) if c.iter().any(|v| !(0.0..=1.0).contains(v)) => return Err("click is a point in the frame, x and y from 0 to 1".into()),
                (None, Some(_)) if surface.is_some() || direction != 0 => return Err("click finds a new surface: to track one again or continue it, give its corners".into()),
                (Some(corners), _) if !klipp_timeline::surface::quad_is_usable(&corners) || corners.iter().flatten().any(|v| !(-1.0..=2.0).contains(v)) => {
                    return Err("the corners must enclose an area, as fractions of the frame: top left, top right, bottom right, bottom left".into());
                }
                _ => {}
            }
            if click.is_some() && (track_corners.is_some() || exclude.is_some()) {
                return Err("track_corners and exclude go with corners: find the surface with click first, then track it again with its corners".into());
            }
            if let Some(t) = track_corners {
                if !klipp_timeline::surface::quad_is_usable(&t) || t.iter().flatten().any(|v| !(-1.0..=2.0).contains(v)) {
                    return Err("track_corners must enclose an area, as fractions of the frame: top left, top right, bottom right, bottom left".into());
                }
            }
            if let Some(bad) = exclude.iter().flatten().position(|e| e.len() < 3 || e.iter().flatten().any(|v| !(-1.0..=2.0).contains(v))) {
                return Err(format!("exclude {bad}: each part left out is a polygon of three or more points, fractions of the frame"));
            }
            let surface = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let clip = seq.tracks.iter().find_map(|t| t.find_clip(id)).ok_or_else(|| format!("no clip with id {clip_id}"))?;
                if clip.media_id().and_then(|m| pool.get(&m)).is_none_or(|m| m.is_still) {
                    return Err("only a clip of moving footage from a file can be tracked".into());
                }
                if let Some(at) = at_secs {
                    let (a, b) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                    if !(a..b).contains(&at) {
                        return Err(format!("at_secs {at:.2} is outside the clip ({a:.2}-{b:.2} s): set the corners on a frame of it"));
                    }
                }
                match surface.as_deref() {
                    Some(n) => Some(
                        klipp_document::ops::find_surface(clip, Some(n))
                            .ok_or_else(|| format!("the clip has no surface \"{n}\""))?
                            .id,
                    ),
                    None => None,
                }
            };
            let request_id = uuid::Uuid::new_v4();
            let mut v = match (corners, click) {
                (Some(corners), _) => send(AppCommand::TrackSurface { clip_id: id, corners, at_secs, name, surface, direction, interactive: false, track: track_corners, exclude })?,
                (None, Some(click)) => send(AppCommand::SnapSurface { clip_id: id, click, at_secs, then_track: true, name, request_id: Some(request_id) })?,
                (None, None) => unreachable!("refused above"),
            };
            if click.is_none() {
                v["text"] = json!("Tracking the surface in the background; it shows under the clip's surfaces in klipp_list_clips when done, one undo step, with where the track was lost if it was. Then pin a picture to it with the pin op.");
                return Ok(v);
            }
            // The click's outcome comes back in a second or two: the
            // corners found (and tracked from), or why none were.
            match wait_for_surface_snap(ctx, request_id) {
                Some(outcome) => {
                    if let Some(why) = outcome["error"].as_str() {
                        return Err(format!("no surface found round the click: {why}. Give its four corners instead, measured with klipp_frame_at; for a plain surface put them on its edges"));
                    }
                    v["corners"] = outcome["corners"].clone();
                    v["text"] = json!("Found the surface round the click (corners, fractions of the frame) and checked that it moves as one plane over the next frames; tracking it in the background. It shows under the clip's surfaces in klipp_list_clips when done, one undo step. Check the corners with klipp_frame_at: a surface's outer edge is found, a screen's bezel included.");
                }
                None => {
                    v["text"] = json!("Finding the surface's edges round the click, then tracking it, in the background: it shows under the clip's surfaces in klipp_list_clips when done. If nothing shows, give its corners.");
                }
            }
            Ok(v)
        }

        ControlCommand::MeasureWater { clip_id } => {
            if in_batch {
                return Err("measure_water runs in the background; send it on its own".into());
            }
            let id = existing_clip_id(ctx, &clip_id)?;
            {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let clip = seq.tracks.iter().find_map(|t| t.find_clip(id)).ok_or_else(|| format!("no clip with id {clip_id}"))?;
                if klipp_render::water::reflection_of(clip).is_none() {
                    return Err("the clip has no reflection: add one with the effect op (kind calm_water) first".into());
                }
            }
            let mut v = send(AppCommand::MeasureWater { clip_id: id })?;
            v["text"] = json!("Measuring the water under the reflection in the background, from the footage below it: a second or two. klipp_list_clips shows its water once stored; where the reflection takes its tint and darkness from the water, they follow.");
            Ok(v)
        }

        ControlCommand::SurfaceKey { clip_id, surface, corners, at_secs } => {
            if in_batch {
                return Err("surface_key solves the track again in the background; send it on its own".into());
            }
            if !klipp_timeline::surface::quad_is_usable(&corners) || corners.iter().flatten().any(|v| !(-1.0..=2.0).contains(v)) {
                return Err("the corners must enclose an area, as fractions of the frame: top left, top right, bottom right, bottom left".into());
            }
            let id = existing_clip_id(ctx, &clip_id)?;
            let surface = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let clip = seq.tracks.iter().find_map(|t| t.find_clip(id)).ok_or_else(|| format!("no clip with id {clip_id}"))?;
                if let Some(at) = at_secs {
                    let (a, b) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                    if !(a..b).contains(&at) {
                        return Err(format!("at_secs {at:.2} is outside the clip ({a:.2}-{b:.2} s): correct it on a frame of the clip"));
                    }
                }
                let s = klipp_document::ops::find_surface(clip, Some(&surface)).ok_or_else(|| format!("the clip has no surface \"{surface}\": track it with track_surface first"))?;
                if s.keys.is_empty() {
                    return Err(format!("the surface \"{}\" isn't tracked yet: track it with track_surface first", s.name));
                }
                s.id
            };
            let mut v = send(AppCommand::CorrectSurface { clip_id: id, surface, corners, at_secs })?;
            v["text"] = json!("Correcting the surface in the background: the frame is held as given and the track either side solved again, one undo step. Its time shows under the surface's corrections in klipp_list_clips when done, with what is still low or lost.");
            Ok(v)
        }

        ControlCommand::SelectClip { clip_id: c } => {
            let id = match c {
                Some(s) => Some(existing_clip_id(ctx, &s)?),
                None => None,
            };
            send(AppCommand::SelectClip(id))
        }

        #[cfg(feature = "test-hooks")]
        ControlCommand::ShowPanel { workspace, inspector_tab } => {
            const WORKSPACES: [&str; 4] = ["edit", "colour", "color", "export"];
            const TABS: [&str; 8] = ["video", "audio", "colour", "color", "effects", "title", "generator", "info"];
            if let Some(w) = workspace.as_deref().filter(|w| !WORKSPACES.contains(w)) {
                return Err(format!("no workspace {w:?}: edit, colour or export"));
            }
            if let Some(t) = inspector_tab.as_deref().filter(|t| !TABS.contains(t)) {
                return Err(format!("no inspector tab {t:?}: video, audio, colour, effects, title, generator or info"));
            }
            send(AppCommand::ShowPanel { workspace, inspector_tab })
        }
        #[cfg(feature = "test-hooks")]
        ControlCommand::EffectCommand { clip_id, action, index, to, effect } => {
            use crate::protocol::EffectAction as A;
            let clip_id = existing_clip_id(ctx, &clip_id)?;
            let effect = || -> Result<klipp_effects::BuiltinEffect, String> {
                serde_json::from_value(effect.clone().ok_or("this action carries an effect")?).map_err(|e| format!("the effect: {e}"))
            };
            // The inspector's drag key for the effect at this place.
            let key = format!("{clip_id:?}:builtin:{index}");
            send(match action {
                A::Add => AppCommand::AddBuiltinEffect { clip_id, effect: effect()? },
                A::Remove => AppCommand::RemoveBuiltinEffect { clip_id, index },
                A::Toggle => AppCommand::ToggleBuiltinEffectEnabled { clip_id, index },
                A::Update | A::DragStep => AppCommand::UpdateBuiltinEffect { clip_id, index, effect: effect()? },
                A::Reorder => AppCommand::ReorderBuiltinEffect { clip_id, from_index: index, to_index: to },
                A::DragBegin => AppCommand::BeginEffectDrag { key, clip_id, effect_index: index, old_effect: effect()? },
                A::DragEnd => AppCommand::EndEffectDrag { key, clip_id, effect_index: index, new_effect: effect()? },
                A::DragAbandon => AppCommand::AbandonEffectDrag { clip_id, effect_index: index, added: false },
            })
        }

        ControlCommand::Plan { ops, detail } => {
            if in_batch {
                return read_in_batch("plan");
            }
            let (plan, revision) = resolve_plan_seeing(ctx, &ops)?;
            let mut v = json!({ "text": if detail { plan.text() } else { plan.text_folded() }, "steps_resolved": plan.steps.len(), "duration_secs": plan.duration_secs, "revision": revision, "applied": false });
            if detail {
                v["steps"] = json!(plan.report());
            }
            with_warnings(&mut v, op_font_warnings(&ops));
            Ok(v)
        }

        ControlCommand::Apply { ops, expected_revision, expected_project_identity, label, detail } => {
            if in_batch {
                return Err("apply is its own batch; send it on its own".into());
            }
            if let Some(want) = expected_project_identity {
                let now = project_status(ctx)?.identity;
                if now != want {
                    return Err(format!("the project changed since this plan was made (expected identity {want}, current identity {now}); plan again"));
                }
            }
            if let Some(want) = expected_revision {
                let now = ctx.shared.revision();
                if now != want {
                    return Err(format!("the document is at revision {now}, the plan was made at {want}; plan again"));
                }
            }
            let (plan, revision) = resolve_plan_seeing(ctx, &ops)?;
            if let Some(want) = expected_revision {
                if revision != want {
                    return Err(format!("the document is at revision {revision}, the plan was made at {want}; plan again"));
                }
            }
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let cut_changes = if ops.iter().any(|op| matches!(op, klipp_document::ops::Op::Cut { .. } | klipp_document::ops::Op::Split { .. })) {
                Some(planned_cut_changes(ctx, &edits)?)
            } else { None };
            let label = label.unwrap_or_else(|| apply_label(&ops));
            let reply = EnvelopeReply { expected_revision: Some(revision), expected_project_identity, label: Some(label), source_indices: None, expected_sources: plan.sources.clone() };
            let mut v = send_with_reply(ctx, Vec::new(), edits, true, reply)?;
            if v["applied"] == true {
                if let Some(changes) = cut_changes {
                    // Each clip's before and after with detail; else which
                    // clips, by id: a cut over a long timeline listed every
                    // one in full (#2492).
                    v["changes"] = if detail { changes["changes"].clone() } else { changes_by_id(&changes["changes"]) };
                    v["shifted"] = changes["shifted"].clone();
                    v["retracked"] = changes["retracked"].clone();
                }
            }
            v["text"] = json!(if detail { plan.text() } else { plan.text_folded() });
            with_warnings(&mut v, op_font_warnings(&ops));
            if detail {
                v["steps"] = json!(plan.report());
            }
            v["duration_secs"] = json!(plan.duration_secs);
            // What the steps made, by id, so the next call needs no read
            // of the timeline to name them (#2364).
            let made = plan.made();
            if !made.is_empty() {
                v["made"] = json!(made);
            }
            let brief = ctx.sequence.read().ok().and_then(|s| s.brief.clone());
            if let Some(next) = next_after_ops(&ops, brief.as_ref(), Trail::read(ctx, &ops)) {
                v["next"] = json!(next);
            }
            let wants_voice = ops.iter().any(|o| matches!(o, klipp_document::ops::Op::CleanAudio { voice: true, .. }));
            if wants_voice && !klipp_audio::dsp::voice_isolation::model_installed() {
                v["next"] = json!("Voice Isolation is on the clip, but its model (8 MB) is not downloaded, so the sound passes through unchanged until it is. Klipp downloads it only when the user asks: tell them to press Download model on the effect in the Audio inspector, or use Preferences → AI → Manage models.");
            }
            Ok(v)
        }

        ControlCommand::TitleTemplates => {
            if in_batch {
                return read_in_batch("title_templates");
            }
            Ok(crate::plans::title_templates(crate::plans::template_dir().as_deref(), crate::plans::composition_dir().as_deref()))
        }

        ControlCommand::SaveTemplate { clip_ids: Some(ids), clip_id: None, name, category, fields, replace } => {
            if in_batch {
                return Err("saving a template is not part of a batch: send it on its own".into());
            }
            let reply = save_composition_template(ctx, &ids, &name, category, fields, replace)?;
            // The Templates tab shows it beside the others.
            let _ = send(AppCommand::ReloadTitleTemplates);
            Ok(reply)
        }

        ControlCommand::SaveTemplate { clip_ids: Some(_), clip_id: Some(_), .. } => Err("give clip_id for a title, or clip_ids for clips saved together, not both".into()),

        ControlCommand::SaveTemplate { clip_id: None, clip_ids: None, .. } => Err("give clip_id (a title) or clip_ids (clips saved together)".into()),

        ControlCommand::SaveTemplate { clip_id: Some(clip_id), clip_ids: None, name, category, fields, replace } => {
            if in_batch {
                return Err("saving a template is not part of a batch: send it on its own".into());
            }
            let id = existing_clip_id(ctx, &clip_id)?;
            let (td, secs) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let c = seq.all_clips().find(|c| c.id == id).ok_or("that clip is gone")?;
                match &c.kind {
                    klipp_timeline::ClipKind::Title(td) => (td.clone(), (c.timeline_out - c.timeline_in).to_secs_f64() as f32),
                    _ => return Err("only a title saves as a template: name a title's clip_id".into()),
                }
            };
            let data = klipp_timeline::template_for_save(&td, secs, fields.as_deref())?;
            let dir = crate::plans::template_dir().ok_or("there is no config folder to save templates in")?;
            let category = category.filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "My Templates".into());
            let path = klipp_timeline::save_template(&dir, &name, &category, &data, replace).map_err(|e| if e.contains("saved already") { format!("{e}: give replace true to save over it, or another name") } else { e })?;
            // The Titles browser shows it beside the others.
            let _ = send(AppCommand::ReloadTitleTemplates);
            let kept: Vec<&str> = data.published.iter().map(|f| f.label.as_str()).collect();
            Ok(json!({
                "text": format!("saved \"{}\" in {category}, keeping {} editable; the template op places it by that name", name.trim(), if kept.is_empty() { "nothing".to_string() } else { kept.join(", ") }),
                "path": path.display().to_string(),
            }))
        }

        ControlCommand::Fonts { fetch: Some(name), .. } => {
            if in_batch {
                return Err("a font fetch is not part of a batch: send it on its own".into());
            }
            fetch_font(&name)
        }

        ControlCommand::Fonts { query, fetch: None } => {
            if in_batch {
                return read_in_batch("fonts");
            }
            let bundled: Vec<String> = klipp_title::bundled::families().into_iter().map(|(name, _)| name.to_owned()).collect();
            let installed: Vec<String> = klipp_title::font_discovery::discover_system_fonts().keys().cloned().collect();
            Ok(fonts_reply(bundled, installed, query.as_deref()))
        }

        ControlCommand::GraphOps { ops, expected_revision, expected_project_identity, label } => {
            if in_batch { return Err("graph_ops is its own atomic batch; send it on its own".into()); }
            crate::graph_request::write(ctx, &ops, expected_revision, expected_project_identity, label)
        }

        ControlCommand::GraphRead { path, depth } => {
            if in_batch { return read_in_batch("graph_read"); }
            let path = crate::graph_prepare::existing_path(&path)?;
            let identity = project_status(ctx)?.identity;
            let sequence = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let revision = ctx.shared.revision();
            let documents = crate::graph_model::documents(&sequence, path.root)?;
            let mut value = crate::graph_read::read(&documents, &path, depth)?;
            drop(sequence);
            if project_status(ctx)?.identity != identity || ctx.shared.revision() != revision {
                return Err("the document changed during the graph read; read again".into());
            }
            value["revision"] = json!(revision);
            value["project_identity"] = json!(identity);
            Ok(value)
        }

        ControlCommand::NodeTypes { category } => {
            if in_batch {
                return read_in_batch("node_types");
            }
            Ok(node_types(category.as_deref()))
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// The node types the registries describe (#1086): the built-in effects and
/// the grade nodes (#1009), each with its id, name and category,
/// ports, and each parameter with the name its keyframes are stored under,
/// its unit, default and ranges. `category` (any case, as the registry
/// names it: Colour, BlurDetail, LightGlow, Geometry, …) narrows the list.
pub fn node_types(category: Option<&str>) -> serde_json::Value {
    use klipp_effects::registry::effect_types;
    let wanted = |c: &str| category.is_none_or(|w| w.eq_ignore_ascii_case(c));
    let ports = |ps: &[klipp_effects::registry::PortDesc], reroute: bool| -> Vec<serde_json::Value> {
        ps.iter()
            .map(|p| {
                let mut port = json!({ "key": p.key, "type": format!("{:?}", p.ty), "optional": p.optional });
                // A reroute carries whatever reaches it (#1877).
                if reroute {
                    port["type"] = json!("Image or Matte: what reaches it");
                }
                if let Some(channel) = p.params.iter().find(|q| q.key == "channel") {
                    port["channels"] = json!(channel.options);
                    port["default_channel"] = json!(channel.default);
                    port["absent_channel"] = json!(channel.migrated);
                }
                if !p.params.is_empty() {
                    // Each saved among the node's parameters under "<port>.<key>".
                    port["params"] = p
                        .params
                        .iter()
                        .map(|q| {
                            json!({
                                "key": q.key,
                                "saved_as": klipp_effects::registry::port_param_key(p.key, q.key),
                                "label": q.label,
                                "options": q.options,
                                "default": q.default,
                                "migrated": q.migrated,
                            })
                        })
                        .collect();
                }
                port
            })
            .collect()
    };
    // The built-in effects, by their saved tag, and the grade nodes, by
    // their saved kind. An effect type in the Grade category is a grade
    // compound's marker in a clip's list (#1746), never added as an
    // effect: not listed.
    let all = effect_types()
        .filter(|t| t.desc.category != klipp_effects::registry::Category::Grade)
        .map(|t| (&t.desc, t.tag.to_owned(), false))
        .chain(klipp_nodes::registry::node_types().map(|t| (&t.desc, format!("{:?}", t.kind), true)));
    let types: Vec<serde_json::Value> = all
        .filter(|(desc, _, _)| wanted(&format!("{:?}", desc.category)))
        .map(|(desc, tag, grade)| {
            let params: Vec<serde_json::Value> = desc
                .params
                .iter()
                .filter(|p| !p.derived)
                .map(|p| {
                    let num = |v: f32| if v.is_finite() { json!(v) } else { serde_json::Value::Null };
                    let mut parameter = json!({
                        "key": p.key,
                        "field": p.field,
                        "label": p.label,
                        "kind": format!("{:?}", p.kind),
                        "unit": format!("{:?}", p.unit),
                        "default": num(p.default),
                        "range": p.range.map(|(a, b)| json!([a, b])),
                        "slider": p.soft.map(|(a, b)| json!([a, b])),
                        "keyframable": p.keyframable,
                    });
                    if desc.id == "klipp.grade.mask" && p.key == "masks" {
                        parameter["value_format"] = json!(crate::graph_masks::FORMAT);
                    }
                    parameter
                })
                .collect();
            let mut node = json!({
                "id": desc.id,
                "tag": tag,
                "name": desc.name,
                "category": format!("{:?}", desc.category),
                "mixable": desc.mixable,
                "inputs": ports(desc.inputs, desc.id == "klipp.grade.reroute"),
                "outputs": ports(desc.outputs, desc.id == "klipp.grade.reroute"),
                "params": params,
            });
            if grade {
                for (side, input) in [("inputs", true), ("outputs", false)] {
                    for (index, port) in node[side].as_array_mut().unwrap().iter_mut().enumerate() {
                        let key = if input { klipp_graph_format::ports::input_key(&tag, index as u8) } else { klipp_graph_format::ports::output_key(&tag, index as u8) };
                        if port["key"] != key.as_str() {
                            port["semantic_key"] = port["key"].clone();
                            port["key"] = json!(key.as_str());
                        }
                    }
                }
            }
            if desc.mixable {
                node["mix"] = json!({"kind":"Float","range":[0,1],"default":1,"operation":"mix"});
            }
            node
        })
        .collect();
    let text = format!(
        "{} node types{}. Each lists its ports and parameters; a parameter's key is the name its keyframes are stored under. A mixable type takes an optional mask input and a mix amount.",
        types.len(),
        category.map(|c| format!(" in {c}")).unwrap_or_default()
    );
    json!({ "text": text, "types": types, "publication": {
        "available":true,
        "scope":"Clip groups; other roots remain subject to their persistence restrictions.",
        "publish":{"op":"publish","path":"group path","from":"direct child path (supports {new:k})","from_port":"child output key","key":"new group output key","label":"optional display name"},
        "unpublish":{"op":"unpublish","path":"group path","key":"published output key"},
        "type":"Inferred from the child's output; cannot be overridden.",
        "read":"graph_read lists group ports with key, direction, ty and child binding; connect from the group path using key.",
        "atomic":"Publish and connect in one graph_ops batch; disconnect readers before unpublishing in the same batch."
    } })
}

#[cfg(test)]
mod node_types_tests {
    use super::node_types;

    /// #1877: a reroute's ports are said to carry what reaches them.
    #[test]
    fn a_reroutes_ports_carry_either_type() {
        let all = node_types(Some("grade"));
        let reroute = all["types"].as_array().unwrap().iter().find(|t| t["tag"] == "Reroute").expect("the reroute is listed");
        for side in ["inputs", "outputs"] {
            assert_eq!(reroute[side][0]["type"], "Image or Matte: what reaches it", "{side}");
        }
    }

    /// Every registered effect type is listed, with its parameters, but a
    /// grade compound's marker (#1746), and a category narrows the list to
    /// its own.
    #[test]
    fn every_registered_type_is_listed_and_a_category_narrows() {
        let all = node_types(None);
        let types = all["types"].as_array().unwrap();
        let effects = klipp_effects::registry::effect_types().filter(|t| t.desc.category != klipp_effects::registry::Category::Grade).count();
        assert_eq!(types.len(), effects + klipp_nodes::registry::node_types().count());
        assert!(types.iter().all(|t| t["tag"] != "grade_compound"), "the marker isn't listed");
        let gamma = types.iter().find(|t| t["tag"] == "gamma").expect("gamma is listed");
        let param = gamma["params"].as_array().unwrap().iter().find(|p| p["key"] == "gamma").expect("its gamma");
        assert_eq!(param["default"], 1.0);
        assert_eq!(param["slider"], serde_json::json!([0.1f32, 5.0]));
        // ChromaKey's soft edge is keyed "spill": the key is what keyframes use.
        let key = types.iter().find(|t| t["tag"] == "chroma_key").unwrap();
        assert!(key["params"].as_array().unwrap().iter().any(|p| p["key"] == "spill" && p["field"] == "soft_edge"));
        let colour = node_types(Some("colour"));
        let colour = colour["types"].as_array().unwrap();
        assert!(!colour.is_empty() && colour.len() < types.len());
        assert!(colour.iter().all(|t| t["category"] == "Colour"));
        assert!(node_types(Some("no such category"))["types"].as_array().unwrap().is_empty());
        // The grade nodes are listed under Grade, by their saved kind.
        let grade = node_types(Some("grade"));
        let grade = grade["types"].as_array().unwrap();
        assert_eq!(grade.len(), klipp_nodes::registry::node_types().count());
        let corrector = grade.iter().find(|t| t["tag"] == "Corrector").expect("the Corrector is listed");
        assert!(corrector["params"].as_array().unwrap().iter().any(|p| p["key"] == "exposure"));
        let mask = grade.iter().find(|t| t["tag"] == "Mask").unwrap();
        assert_eq!(mask["outputs"][0]["key"], "out");
        assert_eq!(mask["outputs"][0]["semantic_key"], "matte");
        assert_eq!(mask["outputs"][0]["type"], "Matte");
    }

    /// What an assistant learns about mixing (#1089, M2): which types are
    /// mixable, and the mask input with its channel setting and where that
    /// is saved. Pinned, so a change to it is a decision.
    #[test]
    fn mixable_types_and_their_mask_input_are_described() {
        let all = node_types(None);
        let types = all["types"].as_array().unwrap();
        let find = |tag: &str| types.iter().find(|t| t["tag"] == tag).unwrap().clone();
        let blur = find("gaussian_blur");
        assert_eq!(blur["mixable"], true);
        assert_eq!(
            blur["inputs"],
            serde_json::json!([
                { "key": "in", "type": "Image", "optional": false },
                { "key": "mask", "type": "Matte", "optional": true,
                  "channels": ["red", "luminance", "alpha"], "default_channel": "luminance", "absent_channel": "red", "params": [
                    { "key": "channel", "saved_as": "mask.channel", "label": "Channel",
                      "options": ["red", "luminance", "alpha"], "default": "luminance", "migrated": "red" }
                ] }
            ])
        );
        let invert = find("invert");
        assert_eq!(blur["mix"], serde_json::json!({"kind":"Float","range":[0,1],"default":1,"operation":"mix"}));
        assert!(invert.get("mix").is_none());
        assert_eq!(all["publication"]["available"], true);
        assert_eq!(invert["mixable"], false);
        assert_eq!(invert["inputs"], serde_json::json!([{ "key": "in", "type": "Image", "optional": false }]));
        // 38 effects (the 3D LUT and the colour-space transform among
        // them, #1746; the flicker and the binocular vignette, #989; the
        // displace, #2702) and the 8 gateable grade corrections (M3 step
        // 17a).
        assert_eq!(types.iter().filter(|t| t["mixable"] == true).count(), 46);
        assert_eq!(find("lut_3d")["mixable"], true);
        assert_eq!(find("colour_space_transform")["mixable"], true);
        assert_eq!(find("flicker")["mixable"], true);
        assert_eq!(find("binocular_vignette")["mixable"], true);
        // A correction's gate reads red until a channel is picked: a new
        // connection writes none, and an absent one is red.
        let corrector = find("Corrector");
        assert_eq!(corrector["mixable"], true);
        assert_eq!(
            corrector["inputs"][1],
            serde_json::json!({ "key": "mask", "type": "Matte", "optional": true,
                "channels": ["red", "luminance", "alpha"], "default_channel": "red", "absent_channel": "red", "params": [
                    { "key": "channel", "saved_as": "mask.channel", "label": "Channel",
                      "options": ["red", "luminance", "alpha"], "default": "red", "migrated": "red" }
                ] })
        );
        let gates = ["Corrector", "ColorBalance", "Exposure", "Contrast", "Saturation", "ColorWheels", "ToneControls", "RGBCurves"];
        for t in types.iter().filter(|t| t["category"] == "Grade") {
            assert_eq!(t["mixable"] == true, gates.contains(&t["tag"].as_str().unwrap()), "{}", t["tag"]);
        }
    }
}

#[cfg(test)]
mod template_name_tests {
    use super::*;

    /// #618: a template is found by its name in any case; a name there
    /// isn't is refused with the names there are.
    #[test]
    fn a_template_is_found_by_name() {
        let dir = std::env::temp_dir().join(format!("klipp-control-templates-{}", uuid::Uuid::new_v4()));
        let seq = klipp_timeline::Sequence::new_default();
        let pool = klipp_media::MediaPool::new();
        let colour = klipp_project::template::TemplateColour { working_colorspace: Default::default(), grade_encoding: Default::default(), global_lut_path: None, global_lut_opacity: 1.0, global_lut_enabled: false };
        let setup = klipp_project::template::ProjectSetup { sequences: std::slice::from_ref(&seq), pool: &pool, colour };
        let path = klipp_project::template::ProjectTemplate::capture("Weekly Show", "", &setup).save(&dir).unwrap();
        assert_eq!(template_named(&dir, "weekly show").unwrap(), path);
        let err = template_named(&dir, "Daily").unwrap_err();
        assert!(err.contains("Weekly Show"), "{err}");
        assert!(template_named(&dir.join("none"), "Daily").unwrap_err().contains("none yet"));
        assert_eq!(dependency_json(&klipp_project::template::Dependency::LinkedMedia { path: "/a.mov".into(), present: false }), json!({ "media": "/a.mov", "copied": false, "missing": true }));
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// How long a click's outcome is waited for before the reply goes without it.
const SURFACE_SNAP_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The outcome of the surface click `request_id` (#2754), once the app
/// has it; `None` when it doesn't come within [`SURFACE_SNAP_WAIT`].
fn wait_for_surface_snap(ctx: &ControlContext, request_id: uuid::Uuid) -> Option<serde_json::Value> {
    let until = std::time::Instant::now() + SURFACE_SNAP_WAIT;
    let id = json!(request_id.to_string());
    while std::time::Instant::now() < until {
        let outcome = ctx.shared.surface_snap.lock().ok()?.clone();
        if let Some(o) = outcome.filter(|o| o["request_id"] == id) {
            return Some(o);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    None
}

/// Clips `ids` of the open sequence saved together as the composition
/// template `name` (#2757): a scene of their own, carried in the file,
/// exposing `fields` by label (texts and media slots when none are named).
fn save_composition_template(ctx: &ControlContext, ids: &[String], name: &str, category: Option<String>, fields: Option<Vec<String>>, replace: bool) -> Result<serde_json::Value, String> {
    use klipp_timeline::composition_template::{template_from_scene, scene_from_clips, CompositionTemplateStore};
    let ids: Vec<klipp_core::ClipId> = ids.iter().map(|s| existing_clip_id(ctx, s)).collect::<Result<_, _>>()?;
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.with_own_grades().into_owned();
    let others: Vec<klipp_timeline::Sequence> = ctx.others.read().map_err(|_| "sequences lock poisoned")?.iter().map(|o| (*o.sequence).clone()).collect();
    let mut all = others;
    all.push(seq.clone());
    let mut scene = scene_from_clips(&seq, &ids).ok_or("none of those clips is on the open sequence")?;
    scene.name = name.trim().to_string();
    let mut template = template_from_scene(&scene, &all, name, fields.as_deref())?;
    template.category = category.filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "My Templates".into());
    let mut store = CompositionTemplateStore::load(crate::plans::composition_dir());
    if let Some(existing) = store.templates().find(|t| t.name.eq_ignore_ascii_case(name.trim())) {
        if !replace {
            return Err(format!("a template called \"{}\" is saved already: give replace true to save over it, or another name", name.trim()));
        }
        template.id = existing.id;
    }
    let exposed: Vec<String> = template.parameters.values().map(|p| p.label.clone()).chain(template.slots.values().map(|s| format!("{} (slot)", s.label))).collect();
    store.save(template)?;
    Ok(json!({ "text": format!("saved {} clip{} as the template \"{}\", exposing {}; the template op places it as one clip", ids.len(), if ids.len() == 1 { "" } else { "s" }, name.trim(), if exposed.is_empty() { "nothing".into() } else { exposed.join(", ") }) }))
}
