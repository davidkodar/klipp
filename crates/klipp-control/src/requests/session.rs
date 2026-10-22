//! Requests about the session itself: ping, status, jobs and operations, the transport, and the clips on the timeline (#826).

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
        ControlCommand::Ping => Ok(json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "build": build_json(),
        })),

        ControlCommand::OperationStatus { operation_id } => {
            if in_batch { return read_in_batch("operation_status"); }
            operations::status(&operation_id)
        }

        ControlCommand::JobStatus { job_id, wait, timeout_ms } => {
            if in_batch { return read_in_batch("job_status"); }
            jobs::status(ctx, job_id.as_deref(), wait, timeout_ms)
        }

        ControlCommand::CancelJob { job_id } => {
            if in_batch { return Err("cancel_job must be sent on its own".into()); }
            let render = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner()).iter()
                .find(|job| job.job_id == job_id).cloned();
            if let Some(render) = render {
                if matches!(render.status.as_str(), "complete" | "failed" | "cancelled") {
                    return Err(format!("render {job_id} is already {}; nothing cancelled", render.status));
                }
                let mut response = send(AppCommand::CancelExportJob { job_id: job_id.clone() })?;
                response["job_id"] = json!(job_id);
                response["cancel_requested"] = json!(response["applied"] == true);
                response["next"] = json!(if response["applied"] == true {
                    "Read job_status until this render reports cancelled or another terminal state."
                } else {
                    "Cancellation is queued for the UI. Resolve operation_id before assuming the render stopped."
                });
                return Ok(response);
            }
            // A caption translation (#2213): stopped between two lines.
            if klipp_ml_candle::translation::jobs::get(&job_id).is_some() {
                klipp_ml_candle::translation::jobs::cancel(&job_id)?;
                return Ok(json!({ "job_id":job_id, "kind":"translation", "cancel_requested":true,
                    "next":"Read job_status until it reports cancelled; nothing is added." }));
            }
            // A subject matte pass (#239): stopped after the frame it is
            // on, every finished frame kept.
            if let Some(media) = job_id.strip_suffix(":subject_matte") {
                let media_id = klipp_core::MediaId(parse_uuid(media, "job_id")?);
                klipp_analysis::service::cancel_subject_matte(media_id)?;
                return Ok(json!({ "job_id":job_id, "kind":"subject_matte", "cancel_requested":true,
                    "next":"Read job_status until it reports cancelled; the frames made so far are kept." }));
            }
            let media_id = klipp_core::MediaId(parse_uuid(&job_id, "job_id")?);
            klipp_analysis::service::cancel_queued(media_id)?;
            Ok(json!({ "job_id":job_id, "kind":"analysis", "state":"cancelled", "cancelled":true,
                "next":"Read job_status to confirm the terminal analysis state." }))
        }

        ControlCommand::Status => {
            if in_batch {
                return read_in_batch("status");
            }
            let active_jobs = jobs::active(ctx);
            let project = project_status(ctx)?;
            let recent_projects = crate::project_access::status_recents(&project);
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            let perf = ctx.perf_ring.lock().ok().and_then(|r| r.latest().cloned());
            let fps = seq.fps.as_f64();
            let presented = fb.shown_presented().map(|t| t.to_secs_f64());
            let playing = ctx.shared.transport.lock().map(|t| t.playing).unwrap_or(false);
            let media = ctx.media_pool.read().map(|p| p.all().count()).unwrap_or(0);
            let mut text = status_text(&StatusFacts {
                build: klipp_core::build_info::get(),
                project: project.path.as_deref(),
                unsaved: project.dirty,
                sequence: &seq,
                media,
                playhead_secs: presented.unwrap_or(0.0),
                playing,
            });
            if !recent_projects.is_empty() {
                text.push_str("\nRecent projects (open_project by name or path):");
                for p in &recent_projects { text.push_str(&format!("\n{} — {}", p["name"], p["path"])); }
            }
            if !active_jobs.is_empty() {
                text.push_str("\nActive work:");
                for job in active_jobs.iter().take(3) {
                    let kind = job["kind"].as_str().unwrap_or("job");
                    let phase = job["phase"].as_str().unwrap_or("working");
                    if let Some(progress) = job["progress"].as_f64() {
                        text.push_str(&format!(" {kind} {:.0}% ({phase});", progress * 100.0));
                    } else {
                        text.push_str(&format!(" {kind} ({phase});"));
                    }
                }
                // The rest counted, so the text and `active_jobs` agree (#1902).
                if active_jobs.len() > 3 {
                    text.push_str(&format!(" and {} more;", active_jobs.len() - 3));
                }
                text.push('\n');
            }
            Ok(json!({
                // What a person is told when they ask whether Klipp is
                // there: in their words, read live (#626).
                "text": text,
                "build": build_json(),
                // Which Klipp answered: with two running, each on its own
                // socket, a client can tell them apart.
                "pid": std::process::id(),
                "revision": ctx.shared.revision(),
                "active_jobs": active_jobs,
                "recent_projects": recent_projects,
                // The edit history's reach and its next steps, for a client
                // checking that something wrote none (M3 step 18).
                "history": {
                    "undo_depth": project.undo_depth,
                    "redo_depth": project.redo_depth,
                    "next_undo": project.undo,
                    "next_redo": project.redo,
                },
                // Every sequence of the project, the open one marked:
                // `switch_sequence` opens another (M3 step 18).
                "sequences": project.sequences.iter().enumerate().map(|(index, s)| json!({
                    "index": index,
                    "id": s.id,
                    "name": s.name,
                    "clips": s.clips,
                    "active": index == project.active_sequence,
                })).collect::<Vec<_>>(),
                "project": {
                    "path": project.path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "unsaved_changes": project.dirty,
                    "unsaved_changes_from": if project.dirty { project.changed_by.clone().or(Some("edits".into())) } else { None },
                },
                "tracks": seq.tracks.len(),
                "clips": seq.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
                "fps": fps,
                "resolution": { "width": seq.resolution.width, "height": seq.resolution.height },
                "duration_secs": seq.total_duration().to_secs_f64(),
                // The multicam group at the frame on screen: its angles
                // and the one in effect, counted from 1 as switch_angle
                // and keys 1-9 count them, and the primary's cut list in
                // timeline seconds.
                "multicam": presented
                    .map(|s| klipp_core::Timecode::from_secs_f64(s, seq.fps.rate()))
                    .and_then(|t| seq.multicam_at(None, t))
                    .map(|m| json!({
                        "angles": m.angles.iter().map(|a| *a as usize + 1).collect::<Vec<_>>(),
                        "active": m.active as usize + 1,
                        "sound": match m.primary.multicam_sound {
                            klipp_core::MulticamSound::Angle(a) => json!(a as usize + 1),
                            klipp_core::MulticamSound::FollowPicture => json!("follow"),
                            klipp_core::MulticamSound::Off => json!("off"),
                        },
                        "cuts": m.primary.multicam_cuts.iter().map(|(at, a)| json!({
                            "at_secs": m.primary.timeline_in.to_secs_f64() + at,
                            "angle": *a as usize + 1,
                        })).collect::<Vec<_>>(),
                    })),
                // The frame on screen: where the playhead was when it was
                // made. Seconds and, at the sequence's rate, frames.
                "playhead_secs": presented,
                "playhead_frame": presented.map(|s| (s * fps).round() as i64),
                "viewer": {
                    "width": fb.width,
                    "height": fb.height,
                    // The newest frame the engine produced, swapped in or
                    // not — what `screenshot` captures.
                    "generation": fb.latest_generation(),
                },
                "perf": perf.map(|p| json!({
                    "width": p.width,
                    "height": p.height,
                    "target_fps": p.target_fps,
                    "decode_ms": p.decode_ms,
                    "grade_ms": p.grade_ms,
                    "total_ms": p.total_ms,
                    "dropped": p.dropped,
                })),
            }))
        }

        ControlCommand::Play => send(AppCommand::Play),

        ControlCommand::Pause => send(AppCommand::Pause),

        ControlCommand::Stop => send(AppCommand::Stop),

        ControlCommand::Seek { secs } => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let fps = seq.fps;
            drop(seq);
            let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
            send(AppCommand::SeekTo(tc))
        }

        ControlCommand::ListClips { full, track, from_secs, to_secs, media_id } => {
            if in_batch {
                return read_in_batch("list_clips");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            // Each clip as the document has it: the engine's copy carries
            // its track's effects and its group's grade too (#846).
            list_clips(&seq.with_own_grades(), &pool, ctx.shared.revision(), &ClipFilter { full, track, from_secs, to_secs, media_id })
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}
