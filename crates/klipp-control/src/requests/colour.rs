//! Colour requests: looks, scene matching, grade chains and nodes, measuring, skin balance, grading to a target, scopes (#826).

#[allow(unused_imports)]
use crate::*;

#[cfg(test)]
#[path = "colour_group_tests.rs"]
mod group_advice_tests;

#[cfg(test)]
#[path = "colour_auto_tests.rs"]
mod auto_colour_tests;

pub(crate) fn respond(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();
    let mut send = sender!(ctx, collect);

    match cmd {
        ControlCommand::AdoptLook { media_id, from_timeline, apply } => {
            if in_batch {
                return Err("adopt_look measures and writes the brief; send it on its own".into());
            }
            use klipp_color::measure::LookTarget;
            // A preset is a guess at what a piece wants. A reference is
            // the answer: the numbers a grade is actually being asked
            // for, lifted off a picture somebody has already approved.
            // This is how a grade is briefed on a real job — "like
            // this" — and the presets exist for when there is no such
            // picture, not the other way round.
            let (target, source) = match (media_id.as_deref(), from_timeline) {
                (Some(id), _) => {
                    let mid = klipp_core::MediaId(id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
                    let (path, filename, dur, prior) = {
                        let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                        let item = pool.get(&mid).ok_or_else(|| format!("no media item {}", mid.0))?;
                        (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64(), item.log_prior())
                    };
                    let report = klipp_analysis::report::MediaAnalysis::load(&path);
                    // A spread across the file, not one frame: a
                    // reference's look is what it does throughout, and
                    // one frame is one moment of its light.
                    let n = 8usize;
                    let mut measures = Vec::new();
                    for i in 0..n {
                        let t = (i as f64 + 0.5) / n as f64 * dur.max(0.1);
                        if let Ok((data, w, h)) = decode_frame_rgba(&path, t) {
                            let faces: Vec<[f32; 4]> = report
                                .framing
                                .as_ref()
                                .and_then(|f| f.people_at(t, 1.5).map(|s| face_or_head_regions(f, s)))
                                .unwrap_or_default();
                            let mut m = klipp_color::measure::measure_frame(&data, w, h, &faces);
                            m.log_prior = prior;
                            measures.push(m);
                        }
                    }
                    // A log reference would teach the grade to be flat.
                    if measures.iter().filter(|m| klipp_color::measure::looks_like_log(m)).count() * 2 > measures.len() {
                        return Err(format!("{filename} looks like camera log with no input transform, so its numbers are the camera's encoding and not a look; set its log format, or pick a finished reference"));
                    }
                    let mut t = LookTarget::from_measures(&format!("like {filename}"), &measures)
                        .ok_or_else(|| format!("no frame of {filename} could be read"))?;
                    // A look with no skin target meters exposure on the
                    // frame's median, which on a dark reference drives
                    // every face far too dark. The commonest cause is
                    // simply that the reference has not been analysed
                    // yet, so its face regions are not known — say so
                    // rather than quietly handing back a look that
                    // cannot expose a face.
                    // A stylised reference can put its faces off the
                    // skin band entirely — Tears of Steel, graded teal
                    // and orange, reads 89°, which is yellow-green and
                    // no complexion at all. Adopting that would aim
                    // every face in the piece at a hue no face has. A
                    // colourist matching to such a reference takes its
                    // contrast and its cast and still keeps skin on the
                    // line; that is what the skin line on a vectorscope
                    // is for.
                    if let Some(hue) = t.skin_hue {
                        if klipp_color::measure::hue_off_skin_band(hue) > 0.0 {
                            t.skin_hue = None;
                            t.name = format!("like {filename} (its own skin sits {hue:.0}°, off the band, so skin stays on the band)");
                        }
                    }
                    if t.skin_hue.is_none() && t.skin_l.is_none() {
                        let state = klipp_analysis::service::status(mid).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                        if state != "done" {
                            return Err(format!(
                                "{filename} has no face regions to read skin from (its analysis is {state}). A look without a skin target meters exposure on the frame's median, which on a dark reference grades every face far too dark. Analyse it first, then adopt_look again."
                            ));
                        }
                        t.name = format!("like {filename} (no face in it)");
                    }
                    (t, filename)
                }
                (None, true) => {
                    let shots: Vec<(f64, f64, String)> = {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                        picture_track(&seq).into_iter()
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| is_picture_clip(c))
                            .map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into())))
                            .take(24)
                            .collect()
                    };
                    if shots.is_empty() {
                        return Err("no picture clip on a video track to take a look from".into());
                    }
                    // The shot already closest to a defensible picture:
                    // the fewest findings, and a face in it if any shot
                    // has one, since a face is what the rest is judged
                    // against.
                    let mut best: Option<(f32, klipp_color::measure::FrameMeasure, String, f64)> = None;
                    for (from, to, name) in &shots {
                        let (frames, _) = measure_clip(ctx, *from, *to, None, Some(6000))?;
                        let (at, m) = median_measure(&frames);
                        if klipp_color::measure::looks_like_log(&m) {
                            continue;
                        }
                        let look = look_for(ctx, at);
                        let score = colour_report(&m, &look, at).len() as f32 + if m.faces.iter().any(|f| f.readable()) { 0.0 } else { 0.5 };
                        if best.as_ref().is_none_or(|b| score < b.0) {
                            best = Some((score, m, name.clone(), at));
                        }
                    }
                    let (_, m, name, at) = best.ok_or("every shot on the timeline looks like ungraded camera log; set the log formats first")?;
                    let t = LookTarget::from_measures(&format!("like {name}"), &[m]).ok_or("the shot could not be read")?;
                    (t, format!("{name} at {at:.2} s"))
                }
                (None, false) => return Err("give a media_id for a reference file, or from_timeline true to take the look off the timeline's own best shot".into()),
            };
            // What the preset library would have said, for comparison:
            // a reference that lands on a preset is worth knowing about,
            // and so is one that lands nowhere near.
            let nearest = LookTarget::PRESETS
                .iter()
                .filter_map(|n| LookTarget::preset(n))
                .filter_map(|p| {
                    let pairs: Vec<f32> = [
                        (p.mid_l, target.mid_l),
                        (p.contrast_range, target.contrast_range),
                        (p.chroma_mean, target.chroma_mean),
                    ]
                    .iter()
                    .filter_map(|(a, b)| Some((a.as_ref()? - b.as_ref()?).abs()))
                    .collect();
                    // A preset that names no numbers — "screencast",
                    // which is a rule about what to judge rather than a
                    // set of targets — has nothing to be near, and
                    // would otherwise win every comparison at zero.
                    (pairs.len() == 3).then(|| (p.name.clone(), pairs.iter().sum::<f32>()))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let describe = format!(
                "black L* {:.0}, mid {:.0}, white {:.0}, 5–95 range {:.0}, chroma {:.0}, neutrals {}{}",
                target.black_l.unwrap_or(0.0),
                target.mid_l.unwrap_or(0.0),
                target.white_l.unwrap_or(0.0),
                target.contrast_range.unwrap_or(0.0),
                target.chroma_mean.unwrap_or(0.0),
                match (target.cast_a, target.cast_b) {
                    (Some(a), Some(b)) if (a * a + b * b).sqrt() >= 1.5 => format!("{:.1} toward {}", (a * a + b * b).sqrt(), klipp_color::measure::cast_name(a, b)),
                    _ => "neutral".to_string(),
                },
                match (target.skin_hue, target.skin_l) {
                    (Some(h), Some(l)) => format!(", skin {h:.0}° at L* {l:.0}"),
                    // The hue was dropped as off the band (a stylised
                    // reference), but its exposure is still a real
                    // reading of where it puts a face.
                    (None, Some(l)) => format!(", faces exposed to L* {l:.0}, and skin held on its own band rather than the reference's"),
                    _ => ", no face in the reference, so exposure is metered on the frame's median rather than on skin".to_string(),
                },
            );
            if !apply {
                return Ok(json!({
                    "text": format!("The look of {}: {describe}. Nearest preset: {}. Run it with apply true to make it the target every colour judgement is held to.", quoted(&source), nearest.as_ref().map(|(n, d)| format!("{n} ({d:.0} away)")).unwrap_or_else(|| "none".into())),
                    "look_target": target,
                    "source": source,
                    "nearest_preset": nearest.as_ref().map(|(n, _)| n.clone()),
                    "applied": false,
                }));
            }
            let mut brief = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone().unwrap_or_default();
            brief.look = Some(target.clone());
            let mut v = handle(ctx, ControlCommand::SetBrief { brief }, &mut None)?;
            v["text"] = json!(format!("Every colour judgement is now held to the look of {}: {describe}. check, grade_to_target and match_scenes all read it from the brief.", quoted(&source)));
            v["look_target"] = serde_json::to_value(&target).unwrap_or(serde_json::Value::Null);
            v["source"] = json!(source);
            v["applied"] = json!(true);
            Ok(v)
        }

        ControlCommand::MatchScenes { apply, rounds, clip_ids } => {
            if in_batch {
                return Err("match_scenes measures and applies; send it on its own".into());
            }
            use klipp_color::measure::{match_distance, LookTarget, MATCH_GOOD, MATCH_WARN, SCENE_SPLIT};
            let shots: Vec<(String, f64, f64, String)> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                picture_track(&seq).into_iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| is_picture_clip(c))
                    .filter(|c| clip_ids.as_ref().is_none_or(|ids| ids.iter().any(|id| *id == c.id.0.to_string())))
                    .map(|c| (
                        c.id.0.to_string(),
                        c.timeline_in.to_secs_f64(),
                        c.timeline_out.to_secs_f64(),
                        c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into()),
                    ))
                    .take(48)
                    .collect()
            };
            if shots.len() < 2 {
                return Err(if clip_ids.is_some() {
                    "matching needs at least two of the selected shots on the picture track it reads (the first video track with pictures)".into()
                } else {
                    "matching needs at least two shots on one video track".into()
                });
            }
            // Every shot read the way a colourist checks one: three
            // frames, the middle reading grading it.
            let mut read: Vec<(usize, f64, klipp_color::measure::FrameMeasure)> = Vec::new();
            for (i, (id, from, to, _)) in shots.iter().enumerate() {
                let (frames, _drift) = measure_clip(ctx, *from, *to, clip_of(id), Some(6000))?;
                let (at, m) = median_measure(&frames);
                read.push((i, at, m));
            }
            // Scenes: a run of shots lit the same way. A cut inside one
            // has to be invisible; a cut between two is a change the
            // viewer is meant to see, so matching across it would be
            // wrong. The split is by how far apart the readings are,
            // not by which file they came from -- one camera card can
            // hold a whole day, and two cameras can shoot one setup.
            let mut scenes: Vec<Vec<usize>> = Vec::new();
            for (i, _, m) in &read {
                let start_new = match scenes.last().and_then(|s| s.last()) {
                    Some(&prev) => match_distance(&read[prev].2, m) > SCENE_SPLIT,
                    None => true,
                };
                if start_new {
                    scenes.push(vec![*i]);
                } else {
                    scenes.last_mut().expect("just pushed").push(*i);
                }
            }
            let mut lines = Vec::new();
            let mut out_scenes = Vec::new();
            let n_rounds = if apply { rounds.unwrap_or(2).clamp(1, 4) } else { 0 };
            let mut fold = UndoFold::new("Match Scene");
            for (s_i, members) in scenes.iter().enumerate() {
                // The hero: the shot the others are matched to. It is
                // the one already closest to the look, because matching
                // a scene to its worst shot drags the whole scene down
                // -- and, among equals, the one with a face in it, since
                // a face is what the matching is judged on.
                let hero = *members
                    .iter()
                    .min_by(|a, b| {
                        let score = |i: &usize| {
                            let (_, at, m) = &read[*i];
                            let look = look_for(ctx, *at);
                            let fails = colour_report(m, &look, *at).len() as f32;
                            let no_face = if m.faces.iter().any(|f| f.readable()) { 0.0 } else { 0.5 };
                            // Distance to the rest of the scene, so the
                            // hero is not an outlier the others chase.
                            let spread: f32 = members.iter().map(|j| match_distance(m, &read[*j].2)).sum::<f32>() / members.len() as f32;
                            fails + no_face + spread * 0.1
                        };
                        score(a).total_cmp(&score(b))
                    })
                    .expect("a scene has members");
                let hero_name = quoted(&shots[hero].3.chars().take(24).collect::<String>());
                lines.push(format!(
                    "Scene {} — {} shot(s), hero {hero_name} at {:.2} s",
                    s_i + 1,
                    members.len(),
                    read[hero].1
                ));
                let mut per_shot = Vec::new();
                for &i in members {
                    let name: String = shots[i].3.chars().take(24).collect();
                    let before = match_distance(&read[hero].2, &read[i].2);
                    let mut done = 0u32;
                    if i != hero && apply && before > MATCH_GOOD {
                        // The hero itself is the target: it already
                        // carries the look, so matching to it is one
                        // decision rather than two.
                        for _ in 0..n_rounds {
                            let Some(target) = LookTarget::from_measures(&format!("match {hero_name}"), &[read[hero].2.clone()]) else { break };
                            let step = klipp_color::measure::primaries_toward(&read[i].2, &target);
                            let moving = step.exposure.abs() > 0.03 || step.lift.abs() > 0.01 || step.gain.abs() > 0.01 || step.temperature.abs() > 0.5 || step.tint.abs() > 0.5 || step.saturation.abs() > 0.03;
                            if !moving {
                                break;
                            }
                            let now = {
                                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                                let cid = klipp_core::ClipId(shots[i].0.parse().map_err(|e| format!("{e}"))?);
                                seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).map(corrector_now).unwrap_or_default()
                            };
                            let op: klipp_document::ops::Op = serde_json::from_value(json!({
                                "op": "grade", "clip_id": shots[i].0,
                                "exposure": (now.exposure + step.exposure).clamp(-6.0, 6.0),
                                "lift": (now.lift + step.lift).clamp(-1.0, 1.0),
                                "gain": (now.gain + step.gain).clamp(-1.0, 1.0),
                                "temperature": (now.temperature + step.temperature).clamp(-50.0, 50.0),
                                "tint": (now.tint + step.tint).clamp(-50.0, 50.0),
                                "saturation": (now.saturation * (1.0 + step.saturation)).clamp(0.0, 4.0),
                            })).map_err(|e| e.to_string())?;
                            let (plan, revision) = resolve_plan(ctx, &[op])?;
                            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                            fold.send(ctx, edits, revision)?;
                            done += 1;
                            read[i].2 = measure_at(ctx, read[i].1, clip_of(&shots[read[i].0].0), Some(6000))?;
                            if match_distance(&read[hero].2, &read[i].2) <= MATCH_GOOD {
                                break;
                            }
                        }
                    }
                    let after = match_distance(&read[hero].2, &read[i].2);
                    let verdict = if i == hero {
                        "the hero".to_string()
                    } else if after <= MATCH_GOOD {
                        format!("matched, ΔE {after:.1}")
                    } else if after <= MATCH_WARN {
                        format!("close, ΔE {after:.1} — a careful eye finds the cut")
                    } else {
                        format!("OFF, ΔE {after:.1} — the cut reads as a change")
                    };
                    lines.push(format!(
                        "  {} at {:.2} s: {verdict}{}",
                        quoted(&name),
                        read[i].1,
                        if done > 0 { format!(" (was {before:.1}, {done} round(s))") } else { String::new() },
                    ));
                    per_shot.push(json!({
                        "clip_id": shots[i].0,
                        "secs": read[i].1,
                        "name": name,
                        "hero": i == hero,
                        "residual_to_hero": after,
                        "residual_before": before,
                        "rounds": done,
                    }));
                }
                out_scenes.push(json!({ "scene": s_i + 1, "hero_clip_id": shots[hero].0, "hero_secs": read[hero].1, "shots": per_shot }));
            }
            // The joins the viewer actually sees: each cut, in order.
            // A shot can sit close to its hero and still cut badly
            // against the shot beside it, and it is the cut that shows.
            let mut joins = Vec::new();
            for w in read.windows(2) {
                let d = match_distance(&w[0].2, &w[1].2);
                let same_scene = scenes.iter().any(|s| s.contains(&w[0].0) && s.contains(&w[1].0));
                if d > MATCH_WARN {
                    let (a, b) = (quoted(&shots[w[0].0].3.chars().take(18).collect::<String>()), quoted(&shots[w[1].0].3.chars().take(18).collect::<String>()));
                    // A join between two scenes is often meant to look
                    // different — a cut to another location should.
                    // But an interview on two cameras, or one where a
                    // lamp was changed half way, splits into "scenes"
                    // the viewer reads as one, and then the boundary is
                    // precisely the fault. So every join is reported
                    // and the person decides which are intended.
                    lines.push(if same_scene {
                        format!("  [warn] the cut from {a} to {b} at {:.2} s is a {d:.1} ΔE step inside one scene — the viewer sees the join", w[1].1)
                    } else {
                        format!("  [note] the cut from {a} to {b} at {:.2} s is a {d:.1} ΔE step between two scenes — meant, if they are different places; a fault if the viewer is meant to read them as one", w[1].1)
                    });
                }
                joins.push(json!({ "at": w[1].1, "residual": d, "same_scene": same_scene }));
            }
            Ok(json!({
                "text": format!(
                    "{}\n\nMatched on what the eye notices across a cut, in its order: exposure first, then how differently each face sits against the skin band once the scene's own cast is taken off, then the cast on the neutrals, then contrast. The faces are compared as errors and not as colours, so two people with different complexions lit the same way still match. Under {MATCH_GOOD:.0} ΔE the cut is invisible; over {MATCH_WARN:.0} ΔE it reads as a change; over {SCENE_SPLIT:.0} ΔE the shots are not the same lighting setup and belong to different scenes.{}",
                    lines.join("\n"),
                    if apply { "" } else { " Run it with apply true to grade each shot onto its scene's hero, as one undo step." },
                ),
                "scenes": out_scenes,
                "joins": joins,
                "applied": apply,
                "undo_steps": fold.finish(ctx),
                "revision": ctx.shared.revision(),
            }))
        }

        ControlCommand::GradeChain { clip_id } => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let want = clip_id.as_deref().map(|s| s.parse::<uuid::Uuid>().map_err(|e| format!("clip_id is not a uuid: {e}"))).transpose()?;
            let clips: Vec<&klipp_timeline::Clip> = seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .flat_map(|t| t.clips.iter())
                .filter(|c| matches!(c.kind, klipp_timeline::ClipKind::Media { .. }) && want.is_none_or(|w| c.id.0 == w))
                .take(48)
                .collect();
            if clips.is_empty() {
                return Err("no picture clip to read a grade chain from".into());
            }
            let mut lines = Vec::new();
            let mut out = Vec::new();
            for c in &clips {
                let name = quoted(&c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into()).chars().take(28).collect::<String>());
                let chain = c.own_grade().chain();
                let faults = c.own_grade().chain_out_of_order();
                let shown: Vec<String> = chain.iter()
                    .filter(|l| !matches!(l.stage, klipp_nodes::GradeStage::Plumbing) || l.kind == klipp_nodes::NodeKind::Source)
                    .map(|l| format!("{} [{}]{}{}", quoted(&l.label), l.stage.label(), if l.gated { ", gated by a key" } else { "" }, if l.enabled { "" } else { ", bypassed" }))
                    .collect();
                lines.push(format!("{name}: {}", if shown.is_empty() { "no grade".to_string() } else { shown.join(" → ") }));
                for f in &faults {
                    lines.push(format!("  [warn] {f}"));
                }
                out.push(json!({
                    "clip_id": c.id.0.to_string(),
                    "name": name,
                    "chain": chain.iter().map(|l| json!({
                        "node_id": l.id.to_string(),
                        "kind": format!("{:?}", l.kind),
                        "label": l.label,
                        "stage": l.stage.label(),
                        "gated": l.gated,
                        "enabled": l.enabled,
                    })).collect::<Vec<_>>(),
                    "out_of_order": faults,
                }));
            }
            Ok(json!({
                "text": format!(
                    "{}\n\nThe order a colourist works in, and why: input transform (the camera's encoding undone, so the numbers below mean something), balance (neutrals neutral, the face exposed, contrast placed — the only step with a right answer), secondary (one thing changed, keyed off a balanced picture), look (no right answer, so last), output transform.",
                    lines.join("\n"),
                ),
                "clips": out,
            }))
        }

        ControlCommand::ColourSelfCheck { size } => {
            if in_batch {
                return read_in_batch("colour_selfcheck");
            }
            // Big enough that a resample cannot smear a patch, small
            // enough to render in a blink: the patches are flat.
            let w = size.unwrap_or(480).clamp(120, 1920);
            let h = (w * 2) / 3;
            let working = klipp_engine::video_decoder::working_colorspace();
            let both = klipp_compose::selfcheck::run_both(w, h, working, klipp_compose::RenderOptions::default());
            let mut lines = Vec::new();
            let mut worst_severity = "ok";
            for (gpu, check) in &both {
                let path = if *gpu { "GPU" } else { "CPU" };
                for (severity, what) in check.findings() {
                    if severity == "error" || (severity == "warning" && worst_severity == "ok") {
                        worst_severity = severity;
                    }
                    lines.push(format!("{path}: {what}"));
                }
            }
            // The two paths against each other: the viewer shows one and
            // an export may take the other, so a disagreement about a
            // flat patch means an approved grade is not the shipped one.
            let (cpu, gpu) = (&both[0].1, &both[1].1);
            let mut worst_pair = (0.0f32, String::new());
            for (a, b) in cpu.patches.iter().zip(gpu.patches.iter()) {
                let de = klipp_color::measure::delta_e2000(a.back, b.back);
                if de > worst_pair.0 {
                    worst_pair = (de, a.name.clone());
                }
            }
            if worst_pair.0 >= 1.0 {
                worst_severity = "error";
                lines.push(format!("the CPU and the GPU disagree about the chart by {:.1} ΔE ({}) — a grade approved in the viewer is not the grade that exports", worst_pair.0, worst_pair.1));
            }
            let trustworthy = both.iter().all(|(_, c)| c.trustworthy()) && worst_pair.0 < 1.0;
            Ok(json!({
                "text": format!(
                    "Colour self-check, a 24-patch ColorChecker at {w}x{h} through the render path in {working:?}:\n{}\n{}",
                    lines.join("\n"),
                    if trustworthy {
                        "Readings from this path can be graded on."
                    } else {
                        "Fix the path before grading to measured targets: every cast and skin reading is partly the pipeline's until this is clean."
                    },
                ),
                "trustworthy": trustworthy,
                "severity": worst_severity,
                "working_colorspace": format!("{working:?}"),
                "cpu": cpu,
                "gpu": gpu,
                "cpu_gpu_max_de": worst_pair.0,
            }))
        }

        ControlCommand::MeasureRegion { secs, x, y, w, h, timeout_ms } => {
            if in_batch {
                return read_in_batch("measure_region");
            }
            for (name, v) in [("x", x), ("y", y)] {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return Err(format!("{name} is a fraction of the frame, 0 to 1"));
                }
            }
            for (name, v) in [("w", w), ("h", h)] {
                if !v.is_finite() || !(0.01..=1.0).contains(&v) {
                    return Err(format!("{name} is a fraction of the frame, above 0 to 1"));
                }
            }
            let (data, fw, fh, _) = grab_picture_rgba(ctx, secs, timeout_ms)?;
            let (cut, cw, ch) = klipp_color::measure::crop(&data, fw as usize, fh as usize, [x, y, w, h]);
            let m = klipp_color::measure::measure_frame(&cut, cw, ch, &[]);
            let look = look_for(ctx, secs);
            let cast = if m.cast() < 3.0 { "neutral".to_string() } else { format!("{:.1} toward {}", m.cast(), klipp_color::measure::cast_name(m.cast_a, m.cast_b)) };
            Ok(json!({
                "text": format!(
                    "The box at {:.0}%,{:.0}% ({:.0}% by {:.0}%) of the frame at {secs:.2} s, {cw}x{ch} px: L* {:.0}/{:.0}/{:.0}, contrast {:.0}, chroma {:.0}, neutrals {cast}{}. Read a face, a sky or a product on its own and compare it with the whole frame (frame_stats).",
                    x * 100.0, y * 100.0, w * 100.0, h * 100.0, m.l_p1, m.l_p50, m.l_p99, m.contrast_range, m.chroma_mean,
                    if m.neutral_solid { "" } else { " (nothing in the box is neutral, so that reading is the box's own colour)" },
                ),
                "lab": m,
                "look": look.name,
                "secs": secs,
                "region": [x, y, w, h],
            }))
        }

        ControlCommand::SkinBalance { clip_id, apply } => {
            if in_batch {
                return Err("skin_balance measures and applies; send it on its own".into());
            }
            use klipp_color::measure::{SKIN_HUE_CENTRE, SKIN_HUE_HIGH, SKIN_HUE_LOW};
            // Read where the face is largest in the clip, not at its
            // midpoint: a face a hundred pixels across is what a skin
            // read needs, and the clip's middle may hold none at all.
            let only = clip_of(&clip_id);
            let (mid, name) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let id = klipp_core::ClipId(clip_id.parse().map_err(|e| format!("clip_id is not a uuid: {e}"))?);
                let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).ok_or_else(|| format!("no clip {clip_id}"))?;
                let item = clip.media_id().and_then(|m| pool.get(&m));
                let name = item.map(|it| quoted(&it.filename)).unwrap_or_else(|| "the clip".into());
                let (t_in, t_out) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
                let biggest = item
                    .and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).framing)
                    .and_then(|f| {
                        f.samples
                            .iter()
                            .filter(|s| s.secs >= s_in && s.secs < s_out)
                            .filter_map(|s| {
                                let area = face_or_head_regions(&f, s).into_iter().map(|b| b[2] * b[3]).fold(0.0f32, f32::max);
                                (area > 0.0).then_some((s.secs, area))
                            })
                            .max_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(secs, _)| t_in + clip.elapsed_for_source_secs(secs, seq.fps.as_f64()))
                    })
                    .filter(|t| *t >= t_in && *t < t_out);
                (biggest.unwrap_or((t_in + t_out) / 2.0), name)
            };
            let look = look_for(ctx, mid);
            let target = look.skin_hue.unwrap_or(SKIN_HUE_CENTRE);
            let mut rounds = Vec::new();
            let mut shift = 0.0f32;
            let mut m = measure_at(ctx, mid, only, Some(6000))?;
            let Some(face0) = m.faces.iter().find(|f| f.readable()).cloned() else {
                return Err(format!("no face to read in {name} at {mid:.2} s, where the largest face in the clip is: the framing analysis found no face there (analyze — the face model comes with the detector), or the face is too small or too dark to read skin in"));
            };
            if face0.mixed_light {
                return Err(format!("{name}: the face is lit by two colours of light (hue spread {:.0}°); a skin secondary would fix one side and break the other — grade the light, not the skin", face0.hue_iqr));
            }
            let scene_cast = m.cast() >= 3.0;
            let same_way = scene_cast && ((face0.hue - target) * klipp_color::measure::Lab { l: 50.0, a: m.cast_a, b: m.cast_b }.hue().signum()) != 0.0 && face0.hue_off_band > 0.0;
            let mut before = json!({ "hue": face0.hue, "chroma": face0.chroma, "l": face0.median.l, "patches": face0.patches, "scene_cast": m.cast(), "scene_cast_toward": klipp_color::measure::cast_name(m.cast_a, m.cast_b) });
            if same_way && face0.hue_off_band > 4.0 {
                before["advice"] = json!("the neutrals carry a cast too: white balance first (grade temperature and tint, or grade_to_target), then skin_balance for what is left");
            }
            // Where the face is, across the shot: the key alone cannot
            // tell a face from anything else the same colour.
            let window = face_track(ctx, &clip_id);
            let windowed = window.len();
            let window = (!window.is_empty()).then(|| serde_json::to_value(&window).unwrap_or(serde_json::Value::Null));
            let n_rounds = if apply { 3 } else { 0 };
            let mut fold = UndoFold::new("Skin Balance");
            for round in 0..n_rounds {
                let Some(face) = m.faces.iter().find(|f| f.readable()) else { break };
                let off = klipp_color::measure::hue_off_skin_band(face.hue);
                let want = klipp_color::measure::hue_diff(face.hue, target);
                if off <= 2.0 && want.abs() <= 4.0 {
                    break;
                }
                // The secondary keys on the wheel's hue, not Lab's, and on
                // the face's own colour: a face with a cast falls outside
                // the generic skin key, which would select nothing and
                // change nothing. The shift is the same two colours'
                // distance on the wheel.
                let now_rgb = klipp_color::measure::lab_to_srgb8(face.median);
                let want_rgb = klipp_color::measure::lab_to_srgb8(klipp_color::measure::lab_at(target, face.chroma.max(6.0), face.median.l));
                let (key_hue, want_hue) = (klipp_color::measure::srgb_hue(now_rgb), klipp_color::measure::srgb_hue(want_rgb));
                shift = (shift + klipp_color::measure::hue_diff(key_hue, want_hue) * 0.8).clamp(-60.0, 60.0);
                // The whole key, and the hue turn alone: a secondary
                // either turns a hue or lifts and saturates, and this one
                // turns skin onto its band.
                let op: klipp_document::ops::Op = serde_json::from_value(json!({
                    "op": "qualify",
                    "clip_id": clip_id,
                    "hue": key_hue,
                    "hue_width": 30.0,
                    "sat_low": 0.12,
                    "sat_high": 1.0,
                    "lum_low": 0.08,
                    "lum_high": 0.95,
                    "hue_shift": shift,
                    "window": window,
                })).map_err(|e| e.to_string())?;
                let (plan, revision) = resolve_plan(ctx, &[op])?;
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                fold.send(ctx, edits, revision)?;
                m = measure_at(ctx, mid, only, Some(6000))?;
                let after = m.faces.iter().find(|f| f.patches >= 4).map(|f| json!({ "hue": f.hue, "chroma": f.chroma }));
                rounds.push(json!({ "round": round + 1, "hue_shift": shift, "after": after }));
            }
            let face_now = m.faces.iter().find(|f| f.readable());
            let text = match face_now {
                Some(f) => format!("{name} at {mid:.2} s: skin hue {:.0}° → {:.0}° (band {SKIN_HUE_LOW:.0}–{SKIN_HUE_HIGH:.0}°, aim {target:.0}°), chroma {:.0}, L* {:.0}, {} of {}×{} patches{}{}", face0.hue, f.hue, f.chroma, f.median.l, f.patches, f.grid, f.grid, if apply { format!("; {} round(s), skin secondary hue shift {shift:+.0}°{}", rounds.len(), match windowed { 0 => ", keyed on hue alone — no face track in this clip's analysis, so anything else in the skin band turns with the face".to_string(), 1 => ", held inside a window on the face".to_string(), n => format!(", held inside a window tracked on the face over {n} samples") }) } else { String::new() }, before.get("advice").and_then(|a| a.as_str()).map(|a| format!(". {a}")).unwrap_or_default()),
                None => format!("{name}: the face could not be read after the change"),
            };
            let undo_steps = fold.finish(ctx);
            Ok(json!({ "text": text, "before": before, "rounds": rounds, "hue_shift": shift, "applied": apply && !rounds.is_empty(), "undo_steps": undo_steps, "revision": ctx.shared.revision(), "next": if apply { "frame_at to see it; check with color reads every shot." } else { "Run it with apply true to turn the skin onto the band as one undo step." } }))
        }

        ControlCommand::GradeToTarget { clip_id, clips, look_if_unbriefed, apply, rounds, force, strength } => {
            if in_batch {
                return Err("grade_to_target measures and applies; send it on its own".into());
            }
            let limits = GradeLimits::named(strength.as_deref().unwrap_or("gentle"))?;
            if let Some(name) = look_if_unbriefed.as_deref().filter(|n| klipp_color::measure::LookTarget::preset(n).is_none()) {
                return Err(format!("{name} is not a look; the presets are {}", klipp_color::measure::LookTarget::PRESETS.join(", ")));
            }
            let shots: Vec<(String, f64, String)> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let want: Vec<uuid::Uuid> = clip_id.iter().chain(&clips).map(|s| s.parse::<uuid::Uuid>().map_err(|e| format!("clip_id is not a uuid: {e}"))).collect::<Result<_, _>>()?;
                // One clip asked for is found on whichever video track
                // holds it; the whole pass takes the picture track.
                // Every visible video track, the asked-for clip or all of
                // them: the top one alone left the main picture under an
                // overlay ungraded (#720). Each is measured alone (#719).
                let tracks: Vec<&klipp_timeline::Track> =
                    seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).collect();
                tracks.into_iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| is_picture_clip(c) && (want.is_empty() || want.contains(&c.id.0)))
                    .map(|c| (c.id.0.to_string(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0, c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into())))
                    .take(48)
                    .collect()
            };
            if shots.is_empty() {
                return Err(match (&clip_id, clips.is_empty()) {
                    (Some(id), true) => format!("{id} is not a picture clip on a visible video track"),
                    (_, false) => "none of those clips is a picture clip on a visible video track".into(),
                    (None, true) => "no picture clip to grade on a video track".into(),
                });
            }
            let n_rounds = if apply { rounds.unwrap_or(3).clamp(1, 5) } else { 0 };
            let mut fold = UndoFold::new(GRADE_TO_TARGET);
            let mut per_shot = Vec::new();
            let mut lines = Vec::new();
            for (id, mid, name) in &shots {
                let look = match look_for(ctx, *mid) {
                    l if l.name == "unbriefed" => look_if_unbriefed.as_deref().and_then(klipp_color::measure::LookTarget::preset).unwrap_or(l),
                    l => l,
                };
                // Three frames across the shot, not one: the middle
                // reading grades it and the drift between them is said.
                let span = shot_span(ctx, id).unwrap_or((*mid - 0.5, *mid + 0.5));
                let (reads, drift) = measure_clip(ctx, span.0, span.1, clip_of(id), Some(6000))?;
                let (mid_t, mut m) = median_measure(&reads);
                let mid = &mid_t;
                // Log with no input transform is not a grading problem.
                if klipp_color::measure::looks_like_log(&m) && !force {
                    let short = quoted(&name.chars().take(28).collect::<String>());
                    lines.push(format!("{short} at {mid:.2} s: left alone — it looks like camera log with no input transform (5–95 range {:.0}, chroma {:.0}). Set the clip's log format so the transform runs, then grade; grade_to_target with force true grades it as it is.", m.contrast_range, m.chroma_mean));
                    per_shot.push(json!({ "clip_id": id, "secs": mid, "skipped": "looks like camera log with no input transform", "lab": m }));
                    continue;
                }
                // The file's own floor, when a floor finding hangs on it:
                // a bright scene is not graded down to a black it does not
                // hold (#1896). The file does not change between rounds.
                let source = if needs_source(&klipp_color::measure::judge(&m, &look), &look) { clip_of(id).and_then(|c| source_reading(ctx, c, *mid)) } else { None };
                m.source = source;
                let before = colour_report(&m, &look, *mid);
                let mut step = klipp_color::measure::primaries_toward(&m, &look);
                let mut done_rounds = 0u32;
                let mut stalled: Option<String> = None;
                // Where the shot started, knobs and colours: the limits
                // and the memory colours are held against this, not
                // against the last round, so small steps cannot add up
                // to an olive sky (#742).
                let cid = klipp_core::ClipId(id.parse().map_err(|e| format!("{e}"))?);
                let start_knobs = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).map(corrector_now).unwrap_or_default()
                };
                let start_m = m.clone();
                let mut held_at_limit: Vec<&'static str> = Vec::new();
                // The first step as it would be taken, inside the limits:
                // what a dry run shows.
                let (first_step, first_held) = limits.clamp(&start_knobs, &start_knobs, &step);
                for _ in 0..n_rounds {
                    let fails = colour_report(&m, &look, *mid);
                    let moving = step.exposure.abs() > 0.03 || step.lift.abs() > 0.01 || step.gain.abs() > 0.01 || step.temperature.abs() > 0.5 || step.tint.abs() > 0.5 || step.saturation.abs() > 0.03;
                    if fails.is_empty() || !moving {
                        break;
                    }
                    let now = {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).map(corrector_now).unwrap_or_default()
                    };
                    let (next, held) = limits.clamp(&start_knobs, &now, &step);
                    for k in held {
                        if !held_at_limit.contains(&k) {
                            held_at_limit.push(k);
                        }
                    }
                    let unchanged = (next.exposure - now.exposure).abs() < 1e-3 && (next.lift - now.lift).abs() < 1e-4 && (next.gain - now.gain).abs() < 1e-4
                        && (next.temperature - now.temperature).abs() < 1e-2 && (next.tint - now.tint).abs() < 1e-2 && (next.saturation - now.saturation).abs() < 1e-3;
                    if unchanged {
                        stalled = Some(format!("stopped at the {} limit for {}: further is past what this strength allows; strength normal or bold goes further, or a group grade, a secondary or a look does it more kindly", limits.name, held_at_limit.join(", ")));
                        break;
                    }
                    let op: klipp_document::ops::Op = serde_json::from_value(json!({
                        "op": "grade", "clip_id": id,
                        "exposure": next.exposure, "lift": next.lift, "gain": next.gain,
                        "temperature": next.temperature, "tint": next.tint, "saturation": next.saturation,
                    })).map_err(|e| e.to_string())?;
                    let (plan, revision) = resolve_plan(ctx, &[op])?;
                    let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                    fold.send(ctx, edits, revision)?;
                    done_rounds += 1;
                    let m_before_round = m.clone();
                    m = measure_at(ctx, *mid, clip_of(id), Some(6000))?;
                    m.source = source;
                    // A round that leaves the shot worse off is not a
                    // step toward anything. The commonest case is a
                    // cast the primaries cannot reach — a tungsten shot
                    // against a daylight target — where the loop pushes
                    // exposure around for ever, crushes the blacks, and
                    // never moves the cast at all. Put the knobs back
                    // where they were and say why it stopped.
                    let after_round = colour_report(&m, &look, *mid);
                    let turned = klipp_color::measure::memory_colours_broken(&start_m, &m, &look);
                    if after_round.len() > fails.len() || turned.is_some() {
                        let op: klipp_document::ops::Op = serde_json::from_value(json!({
                            "op": "grade", "clip_id": id,
                            "exposure": now.exposure, "lift": now.lift, "gain": now.gain,
                            "temperature": now.temperature, "tint": now.tint, "saturation": now.saturation,
                        })).map_err(|e| e.to_string())?;
                        let (plan, revision) = resolve_plan(ctx, &[op])?;
                        let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                        fold.send(ctx, edits, revision)?;
                        // Put back exactly, so read as it was: a measure
                        // taken now could still see the frame the undone
                        // round drew, and said "4 before, 7 after" of a
                        // clip left with its 4 (#516).
                        m = m_before_round;
                        done_rounds -= 1;
                        stalled = Some(match turned {
                            Some(why) => format!("stopped after {done_rounds} round(s): the next one turned colours an eye knows — {why} — so it was put back. The look's numbers are not worth that; hold the shot there, or reach the rest with a secondary that leaves those colours out"),
                            None => format!(
                                "stopped after {done_rounds} round(s): the next one made it worse ({} findings against {}), so it was put back. What is left is past what the primaries can reach — {}",
                                after_round.len(),
                                fails.len(),
                                if m.cast() >= 3.0 { "the cast needs the white balance set at the source, or a qualified secondary" } else { "try a secondary, or accept it" },
                            ),
                        });
                        break;
                    }
                    step = klipp_color::measure::primaries_toward(&m, &look);
                }
                // Short of the target because brighter would blow the
                // highlights: said, with what does reach it (#501).
                if step.held_for_highlights && stalled.is_none() {
                    stalled = Some("held short of the exposure target: brighter would take more than a twentieth of the frame to white. Lift the face on its own, with a window or a secondary, rather than the whole picture".into());
                }
                if stalled.is_none() && !held_at_limit.is_empty() {
                    stalled = Some(format!("held at the {} limit for {}", limits.name, held_at_limit.join(", ")));
                }
                let after = colour_report(&m, &look, *mid);
                let short = quoted(&name.chars().take(28).collect::<String>());
                // The dry run says the step, not only the problems.
                let first = if apply {
                    String::new()
                } else if first_held.is_empty() {
                    format!("; first step: {}", describe_step(&start_knobs, &first_step))
                } else {
                    format!("; first step: {} (held at the {} limit for {})", describe_step(&start_knobs, &first_step), limits.name, first_held.join(", "))
                };
                lines.push(format!(
                    "{short} at {mid:.2} s [{}]: {} before, {} after{}{first}{}{}",
                    quoted(&look.name),
                    before.len(),
                    after.len(),
                    if done_rounds > 0 { format!(" ({done_rounds} round(s))") } else { String::new() },
                    if after.is_empty() { "; on target".to_string() } else { format!("; left: {}", after.iter().map(|f| f.2.clone()).collect::<Vec<_>>().join(", ")) },
                    match (&drift, &stalled) {
                        (Some(d), Some(s)) => format!("; {d}; {s}"),
                        (Some(d), None) => format!("; {d}"),
                        (None, Some(s)) => format!("; {s}"),
                        (None, None) => String::new(),
                    },
                ));
                per_shot.push(json!({ "clip_id": id, "secs": mid, "look": look.name, "before": before.iter().map(|f| json!({ "severity": f.0, "what": f.2, "fix": f.3 })).collect::<Vec<_>>(), "after": after.iter().map(|f| json!({ "severity": f.0, "what": f.2, "fix": f.3 })).collect::<Vec<_>>(), "rounds": done_rounds, "next_step": step, "first_step": first_step, "held_at_limit": held_at_limit, "stalled": stalled, "drift": drift, "frames_read": reads.len(), "lab": m }));
            }
            // Shots of one file are one scene: one grade on them all keeps
            // them alike at every cut, where a primary each drifts apart
            // chasing the target shot by shot (#742).
            // Grouped by the media itself, not its file name: two cards'
            // IMG_0001.mov are two scenes (#770 review).
            // A scene whose shots already share one grade group is graded
            // together: the advice would only repeat after grouping (#1571).
            let (media_of, grouped) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let clips = || seq.tracks.iter().flat_map(|t| t.clips.iter());
                let media_of: std::collections::HashMap<String, klipp_core::MediaId> = clips().filter_map(|c| c.media_id().map(|m| (c.id.0.to_string(), m))).collect();
                let grouped: std::collections::HashMap<String, klipp_core::GradeGroupId> = clips()
                    .filter_map(|c| c.grade_group.filter(|g| seq.grade_groups.iter().any(|x| x.id == *g)).map(|g| (c.id.0.to_string(), g)))
                    .collect();
                (media_of, grouped)
            };
            let one_group = |g: &serde_json::Value| {
                let mut ids = g["clips"].as_array().into_iter().flatten().map(|id| id.as_str().and_then(|id| grouped.get(id)));
                ids.next().flatten().is_some_and(|first| ids.all(|other| other == Some(first)))
            };
            let groups: Vec<serde_json::Value> = scene_groups(&shots, &media_of).into_iter().filter(|g| !one_group(g)).collect();
            for g in &groups {
                lines.push(format!(
                    "{} shots come from {}: one scene. Grade them together (grade_group with these clips, then grade on the group) and keep per-shot moves small; a primary each, chasing the target alone, pulls them apart at the cuts.",
                    g["clips"].as_array().map_or(0, |a| a.len()),
                    quoted(g["file"].as_str().unwrap_or("")),
                ));
            }
            // A preset is a guess at what the piece wants; a reference
            // is what it was actually asked for.
            let briefed = ctx.sequence.read().ok().and_then(|s| s.brief.as_ref().and_then(|b| b.look.clone())).is_some();
            if !briefed {
                lines.push("These are held to a preset, because the brief names no look. adopt_look takes the numbers off a reference — a file you are matching, or this timeline's own best shot — and that is what a grade is normally briefed against.".to_string());
            }
            let undo_steps = fold.finish(ctx);
            if undo_steps > 1 {
                lines.push(format!("The pass is {undo_steps} undo steps, not one: the document changed while it ran."));
            }
            // Applied only where it changed a grade: a pass that found every
            // shot on target, or held, wrote nothing (#2492).
            let changed = apply && undo_steps > 0;
            if apply && !changed {
                lines.push("Nothing was changed: no shot needed a step it could take.".to_string());
            }
            Ok(json!({ "text": lines.join("\n"), "shots": per_shot, "scene_groups": groups, "strength": limits.name, "applied": changed, "undo_steps": undo_steps, "revision": ctx.shared.revision(), "look_from_brief": briefed, "next": if apply { "check with color reads every shot again; match_scenes holds the shots to each other, which is what the eye sees at a cut; skin_balance for a face still off the band; a look (klipp_look) for style, after the grade." } else { "That is where every shot stands against the look and the first step each would take. Run it with apply true to grade them, as one undo step." } }))
        }

        ControlCommand::GetGrade { clip_id: c } => {
            let want = clip_id(&c)?;
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let (track, clip) = seq
                .tracks
                .iter()
                .find_map(|t| t.clips.iter().find(|cl| cl.id == want).map(|cl| (t, cl)))
                .ok_or_else(|| format!("no clip with id {c}"))?;
            // The timeline grade runs on every frame after the clip's own,
            // so what the viewer shows is both; say so, or a reader of this
            // graph alone would misjudge the picture.
            let timeline = seq.timeline_grade().filter(|g| !g.is_identity());
            // Likewise its grade group's grades, before and after its own (#237),
            // and its track's grade after those, where the render copy runs it (#1072).
            let group = clip.grade_group.and_then(|g| seq.grade_group(g)).filter(|g| g.grades());
            let track_grade = track.effects.grade_onto(track.kind, clip);
            Ok(json!({
                "clip_id": c,
                "is_identity": clip.own_grade().is_identity(),
                "graph": serde_json::to_value(clip.own_grade())
                    .map_err(|e| format!("serialising graph: {e}"))?,
                "grade_group": group.map(|g| json!({
                    "name": g.name,
                    "pre": g.stage(klipp_timeline::GradeStage::Pre).filter(|p| !p.is_identity()),
                    "post": g.stage(klipp_timeline::GradeStage::Post).filter(|p| !p.is_identity()),
                })),
                "track_grade": track_grade
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|e| format!("serialising the track grade: {e}"))?,
                "timeline_graph": timeline
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|e| format!("serialising the timeline graph: {e}"))?,
            }))
        }

        ControlCommand::AddNode { clip_id: c, kind, x, y } => {
            let clip_id = existing_clip_id(ctx, &c)?;
            // An OFX plug-in by its identifier ("ofx:net.sf.openfx.invertPlugin"),
            // at the end of the clip's plug-ins, switched on. One that isn't
            // installed is added as asked: it declines as a plug-in missing,
            // visibly, as one opened from a project would.
            if let Some(plugin) = kind.strip_prefix("ofx:") {
                if plugin.trim().is_empty() {
                    return Err("ofx: takes the plug-in's identifier, e.g. ofx:net.sf.openfx.invertPlugin".into());
                }
                return send(AppCommand::AddEffect { clip_id, plugin_name: plugin.to_owned() });
            }
            if klipp_nodes::kinds::addable_node_kind(&kind).is_none() {
                let valid: Vec<&str> = klipp_nodes::kinds::ADDABLE_NODE_KINDS.iter().map(|(n, _)| *n).collect();
                return Err(format!("no node kind {kind:?}; the kinds are {}", valid.join(", ")));
            }
            // The id is chosen here so the reply can say it: the app
            // makes the node with it.
            let node_id = uuid::Uuid::new_v4();
            let mut reply = send(AppCommand::AddNode { clip_id, kind, node_id: Some(node_id), position: (x, y) })?;
            if let Some(r) = reply.as_object_mut() {
                r.insert("node_id".into(), json!(node_id.to_string()));
            }
            Ok(reply)
        }

        ControlCommand::RemoveNode { clip_id: c, node_id } => {
            let clip = existing_clip_id(ctx, &c)?;
            send(AppCommand::RemoveNode {
                clip_id: clip,
                node_id: existing_node_id(ctx, clip, &node_id, "node_id")?,
            })
        }

        ControlCommand::ConnectNodes { clip_id: c, from, from_port, to, to_port } => {
            let clip = existing_clip_id(ctx, &c)?;
            let (from, to) = (existing_node_id(ctx, clip, &from, "from")?, existing_node_id(ctx, clip, &to, "to")?);
            in_one_graph(ctx, clip, from, to)?;
            send(AppCommand::ConnectNodes { clip_id: clip, from, from_port, to, to_port })
        }

        ControlCommand::SetNodeParam { clip_id: c, node_id, param, value, at, interpolation, remove } => {
            let clip = existing_clip_id(ctx, &c)?;
            let node_id = existing_node_id(ctx, clip, &node_id, "node_id")?;
            // A name the params do not carry would be dropped by
            // `apply_override`'s fall-through and look exactly like a
            // grade that refused to work.
            if !klipp_nodes::params::NodeParams::accepts(&param) {
                return Err(format!(
                    "no grade parameter called {param:?}. Names are the ones \
                     klipp_get_grade reports, e.g. exposure, contrast, \
                     temperature, tint, saturation, vibrance, clarity, \
                     highlights, shadows, whites, blacks, lift_x, gain_master, \
                     qual_hue_center, luma_key_low.",
                ));
            }
            let Some(at) = at else {
                if remove || interpolation.is_some() {
                    return Err("remove and interpolation are a key's: give at, the key's time in seconds on the timeline".into());
                }
                let value = value.ok_or("missing value: the number to set")?;
                return send(AppCommand::SetNodeParam { clip_id: clip, node_id, param, value });
            };
            let key = node_key(ctx, clip, node_id, &param, at, value, interpolation.as_deref(), remove)?;
            if in_batch {
                // Into the batch's one step, in order.
                for c in key.commands {
                    send(c)?;
                }
                return Ok(json!({ "queued": true }));
            }
            crate::envelope::send_envelope(ctx, key.commands, false).map(|mut reply| {
                if let Some(o) = reply.as_object_mut() {
                    o.insert("key".into(), key.reply);
                }
                reply
            })
        }

        ControlCommand::ToggleNodeEnabled { clip_id: c, node_id } => {
            let clip = existing_clip_id(ctx, &c)?;
            send(AppCommand::ToggleNodeEnabled {
                clip_id: clip,
                node_id: existing_node_id(ctx, clip, &node_id, "node_id")?,
            })
        }

        ControlCommand::RenameNode { clip_id: c, node_id, name } => {
            let clip = existing_clip_id(ctx, &c)?;
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err("give the node a name that is not blank".into());
            }
            send(AppCommand::RenameNode {
                clip_id: clip,
                node_id: existing_node_id(ctx, clip, &node_id, "node_id")?,
                label: name,
            })
        }

        ControlCommand::SetLogFormat { clip_id: c, format } => {
            let log_format = parse_log_format(&format)?;
            send(AppCommand::SetClipLogFormat { clip_id: existing_clip_id(ctx, &c)?, log_format })
        }

        ControlCommand::Looks { category } => {
            if in_batch {
                return read_in_batch("looks");
            }
            let looks = scan_looks();
            if looks.is_empty() {
                return Err("the bundled LUT library was not found next to the app".into());
            }
            let want = category.as_ref().map(|c| c.to_lowercase());
            let mut text = String::from("Bundled looks (apply with look, name is category/name or any words that pick one out):
");
            let mut cats = serde_json::Map::new();
            for (dir, label) in LOOK_CATEGORIES {
                if let Some(w) = &want {
                    if !dir.contains(w.as_str()) && !label.to_lowercase().contains(w.as_str()) { continue; }
                }
                let names: Vec<String> = looks.iter().filter(|(c, _, _)| c == dir).map(|(_, s, _)| s.clone()).collect();
                if names.is_empty() { continue; }
                let show = if want.is_some() { names.len() } else { 6 };
                let sample = names.iter().take(show).cloned().collect::<Vec<_>>().join(", ");
                let more = if names.len() > show { format!(" (+{} more)", names.len() - show) } else { String::new() };
                text.push_str(&format!("  {label} ({}): {sample}{more}
", names.len()));
                if *dir == "cinematic" {
                    text.push_str("    These convert a camera's log or flat profile (DJI D-Cinelike, a normal profile, Gyroflow's legal range) to normal; on footage shot normally they lift the blacks and wash it out.\n");
                }
                cats.insert(dir.to_string(), json!({ "label": label, "count": names.len(), "names": names }));
            }
            Ok(json!({ "text": text, "categories": cats, "total": looks.len(), "next": "Apply one with look {clip_id, name}. Pass a category name here to see all of its looks." }))
        }

        ControlCommand::Look { clip_id: c, on, name, opacity, remove } => {
            use klipp_document::ops::{ClipRef, Op};
            // A clip, or with `on` the timeline or a grade group (#237).
            let target = match (&c, &on) {
                (_, Some(_)) => None,
                (Some(c), None) => Some(ClipRef::Id(clip_id(c)?)),
                (None, None) => return Err("give clip_id, or on: \"timeline\" or {\"group\": name}".into()),
            };
            let op = if remove {
                Op::Look { clip_id: target, on, lut_path: String::new(), name: None, opacity: None, remove: true }
            } else {
                let name = name.ok_or("give a look name (see looks), or remove to clear")?;
                let (path, display) = resolve_look(&name)?;
                Op::Look { clip_id: target, on, lut_path: path.to_string_lossy().to_string(), name: Some(display), opacity, remove: false }
            };
            // In a batch, one look on many clips is one call and one undo
            // step (#458): the looks resolve together when it is sent.
            if let Some(buf) = collect.as_mut() {
                buf.ops.push(op);
                return Ok(json!({ "queued": true }));
            }
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(plan.steps.first().map(|s| s.what.clone()).unwrap_or_else(|| "look".into())))?;
            v["text"] = json!(plan.text_folded());
            v["next"] = json!("Look on. See it with frame_at or screenshot; check color reads the graded frame. Fade it with opacity, or remove to clear.");
            Ok(v)
        }

        ControlCommand::MatchGrade { clip_id: c, reference_clip_id: r, secs, reference_secs } => {
            if in_batch {
                return Err("match_grade applies a grade; send it on its own, not in a batch".into());
            }
            let target_id = clip_id(&c)?;
            let ref_id = clip_id(&r)?;
            // The two midpoints, and a name for the reply — read, then let the lock go.
            let (t_mid, r_mid, t_name) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let find = |id: klipp_core::ClipId| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(move |cl| cl.id == id).cloned();
                let tc = find(target_id).ok_or_else(|| format!("no clip with id {c}"))?;
                let rc = find(ref_id).ok_or_else(|| format!("no clip with id {r}"))?;
                for (cl, which) in [(&tc, "target"), (&rc, "reference")] {
                    if !matches!(cl.kind, klipp_timeline::ClipKind::Media { .. }) {
                        return Err(format!("the {which} clip is not a picture clip; match the grade of two shots"));
                    }
                }
                // The frame to read: the one asked for, inside the clip;
                // else the middle of the clip's longest detected shot. The
                // clip's own middle could fall past a cut inside it, and
                // then the "reference" was a different shot (#329).
                let at = |cl: &klipp_timeline::Clip, asked: Option<f64>, which: &str| -> Result<f64, String> {
                    let (a, b) = (cl.timeline_in.to_secs_f64(), cl.timeline_out.to_secs_f64());
                    match asked {
                        Some(t) if t >= a && t < b => Ok(t),
                        Some(t) => Err(format!("{which} secs {t:.2} is outside the clip ({a:.2}–{b:.2} s)")),
                        None => {
                            let spans = clip_shot_spans(cl, &pool, seq.fps.as_f64(), 1);
                            Ok(spans.first().map(|(x, y)| (x + y) / 2.0).unwrap_or((a + b) / 2.0))
                        }
                    }
                };
                let name = pool.get(&tc.media_id().unwrap_or(klipp_core::MediaId(uuid::Uuid::nil()))).map(|m| m.filename.clone()).unwrap_or_else(|| "the clip".into());
                (at(&tc, secs, "the target's")?, at(&rc, reference_secs, "the reference's")?, name)
            };
            let (td, tw, th) = grab_frame_rgba_of(ctx, t_mid, clip_of(&c), Some(4000))?;
            let target_stats = frame_stats(&td, tw, th, t_mid);
            let (rd, rw, rh) = grab_frame_rgba_of(ctx, r_mid, clip_of(&r), Some(4000))?;
            let ref_stats = frame_stats(&rd, rw, rh, r_mid);
            let mut knobs = match_grade_knobs(&target_stats, &ref_stats);
            let held = hold_match_for_highlights(&mut knobs, &klipp_color::measure::measure_frame(&td, tw, th, &[]));
            if knobs.is_empty() {
                if let Some(why) = &held {
                    return Ok(json!({
                        "text": format!("{t_name} left as it is: {why}.
target:    {}
reference: {}", target_stats["text"].as_str().unwrap_or(""), ref_stats["text"].as_str().unwrap_or("")),
                        "applied": false,
                        "held_for_highlights": why,
                        "target": target_stats, "reference": ref_stats,
                    }));
                }
                return Ok(json!({
                    "text": format!("{t_name} already matches the reference within tolerance; nothing to change.
target:    {}
reference: {}", target_stats["text"].as_str().unwrap_or(""), ref_stats["text"].as_str().unwrap_or("")),
                    "applied": false,
                    "target": target_stats, "reference": ref_stats,
                }));
            }
            use klipp_document::ops::{ClipRef, Op};
            let op = Op::Grade {
                clip_id: Some(ClipRef::Id(target_id)),
                on: None,
                exposure: knobs.exposure, contrast: None, saturation: None,
                temperature: knobs.temperature, tint: knobs.tint,
                lift: knobs.lift, gamma: None, gain: knobs.gain,
                highlights: None, shadows: None, vibrance: None, reset: false, from: None,
            };
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(format!("match {t_name}")))?;
            let mut moved: Vec<String> = Vec::new();
            for (name, val) in [("exposure", knobs.exposure), ("lift", knobs.lift), ("gain", knobs.gain), ("temperature", knobs.temperature), ("tint", knobs.tint)] {
                if let Some(x) = val { moved.push(format!("{name} {x:+.2}")); }
            }
            v["text"] = json!(format!(
                "Matched {t_name} to the reference: {}.
target was:  {}
reference:   {}",
                moved.join(", "),
                target_stats["text"].as_str().unwrap_or(""),
                ref_stats["text"].as_str().unwrap_or(""),
            ));
            if let Some(why) = &held {
                v["text"] = json!(format!("{}\n[held] {why}", v["text"].as_str().unwrap_or("")));
            }
            v["held_for_highlights"] = json!(held);
            v["target_before"] = target_stats;
            v["reference"] = ref_stats;
            v["knobs_moved"] = json!(moved);
            v["next"] = json!("A first pass. Run check with color, or frame_stats on both shots, to see the match; nudge exposure/temperature/tint with grade if it is not there yet.");
            Ok(v)
        }

        ControlCommand::Scope { mode, secs, path, timeout_ms } => {
            if in_batch {
                return read_in_batch("scope");
            }
            let name = mode.as_deref().unwrap_or("waveform").to_lowercase();
            let m: u8 = match name.as_str() {
                "waveform" | "wave" | "wfm" => 0,
                "parade" | "rgb_parade" | "rgb" => 1,
                "vectorscope" | "vector" | "vscope" => 2,
                "histogram" | "hist" => 3,
                _ => return Err(format!("\"{name}\" is not a scope; use waveform, parade, vectorscope or histogram")),
            };
            let secs = match secs {
                Some(s) => s,
                None => {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    fb.shown_presented().map(|t| t.to_secs_f64()).unwrap_or(0.0)
                }
            };
            let (data, w, h, alone) = grab_scope(ctx, m, secs, timeout_ms)?;
            let out = match path {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                    std::env::temp_dir().join(format!("klipp-scope-{name}-{ts}.png"))
                }
            };
            write_png(&out, &data, w, h)?;
            Ok(json!({
                "path": out.to_string_lossy(),
                "width": w, "height": h, "mode": name, "secs": secs,
                "text": format!("{name} scope of {} at {secs:.2} s, {w}x{h}, written to {}", if alone { "the picture clip alone (titles above it and its fill left out)" } else { "the frame" }, out.display()),
                "next": "The scope is the colourist's second eye; frame_stats gives the same frame as numbers to grade to.",
            }))
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// A grade parameter's key as the socket writes it (#891): its commands,
/// one undo step, and what the reply says of it.
struct NodeKey {
    commands: Vec<AppCommand>,
    reply: serde_json::Value,
}

/// The commands that key `param` of `node_id` on `clip` at `at` seconds
/// on the timeline: a key with `value` (its interpolation set when one is
/// named), or the key there taken away. The time is the clip's own, as
/// keys are kept; a time off the clip is refused.
#[allow(clippy::too_many_arguments)]
fn node_key(ctx: &ControlContext, clip: klipp_core::ClipId, node_id: uuid::Uuid, param: &str, at: f64, value: Option<f32>, interpolation: Option<&str>, remove: bool) -> Result<NodeKey, String> {
    use klipp_core::{Interpolation, KeyEase};
    if !at.is_finite() {
        return Err("at must be a number of seconds".into());
    }
    let (start, end) = {
        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
        let c = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip).ok_or("no such clip")?;
        (c.timeline_in.to_secs_f64(), c.timeline_in.to_secs_f64() + c.duration().to_secs_f64())
    };
    if at < start - 1e-6 || at > end + 1e-6 {
        return Err(format!("at {at} s is off the clip, which runs {start:.3} to {end:.3} s on the timeline"));
    }
    let time_secs = (at - start).max(0.0);
    let param = param.to_string();
    let mut commands = vec![AppCommand::BeginNodeParamEdit { clip_id: clip }];
    if remove {
        commands.push(AppCommand::RemoveKeyframe { clip_id: clip, node_id, param: param.clone(), time_secs });
    } else {
        let value = value.ok_or("missing value: the number to key")?;
        commands.push(AppCommand::AddKeyframe { clip_id: clip, node_id, param: param.clone(), time_secs, value });
        let shape = match interpolation {
            None => None,
            Some("linear") => Some(Ok(Interpolation::Linear)),
            Some("hold") => Some(Ok(Interpolation::Step)),
            Some("bezier") => Some(Ok(Interpolation::Bezier)),
            Some("ease_in") => Some(Err(KeyEase::In)),
            Some("ease_out") => Some(Err(KeyEase::Out)),
            Some("ease_both") => Some(Err(KeyEase::Both)),
            Some(other) => return Err(format!("no interpolation {other:?}: linear, ease_in, ease_out, ease_both, bezier or hold")),
        };
        match shape {
            Some(Ok(interpolation)) => commands.push(AppCommand::SetKeyframeInterpolation { clip_id: clip, node_id, param: param.clone(), time_secs, interpolation }),
            Some(Err(ease)) => commands.push(AppCommand::SetKeyframeEase { clip_id: clip, node_id, param: param.clone(), time_secs, ease }),
            None => {}
        }
    }
    commands.push(AppCommand::EndNodeParamEdit { clip_id: clip });
    Ok(NodeKey { commands, reply: json!({ "param": param, "clip_secs": time_secs, "removed": remove }) })
}
