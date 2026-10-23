//! Review requests: reviewing a draft, narration, recording, and screenshots of the viewer and the window (#826).

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
        ControlCommand::Review { max_width, frames, cols, path } => {
            if in_batch {
                return Err("review renders and reads; send it on its own".into());
            }
            let sheet_out = temp_output(path, "klipp-review", "jpg")?;
            let draft = sheet_out.with_extension("mp4");
            let (w, h, duration) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let (sw, sh) = (seq.resolution.width.max(16), seq.resolution.height.max(9));
                let w = max_width.unwrap_or(640).clamp(160, 1920);
                let h = ((w as f64 * sh as f64 / sw as f64 / 2.0).round() as u32 * 2).max(2);
                (w, h, seq.total_duration().to_secs_f64())
            };
            if duration <= 0.0 {
                return Err("the timeline is empty; nothing to review".into());
            }
            let _ = std::fs::remove_file(&draft);
            handle(ctx, ControlCommand::Export { path: draft.to_string_lossy().to_string(), width: Some(w), height: Some(h), fps: None, crf: Some(30), bit_depth: None, target_lufs: None, true_peak_dbtp: None, burn_captions: None, captions_608: None, hdr10plus: None, hdr: None, peak_nits: None, preset: None, codec: None, audio_codec: None, bitrate_kbps: None, encoder_speed: None }, &mut None)?;
            let started = std::time::Instant::now();
            // The draft, waited for: a review is the picture as rendered.
            let rendered = || -> Result<klipp_core::ExportStatus, String> {
                loop {
                    let jobs = ctx.shared.exports.lock().map_err(|_| "export status lock poisoned")?;
                    let job = jobs.iter().find(|j| j.path == draft).cloned();
                    drop(jobs);
                    match job {
                        Some(j) if j.status == "complete" => return Ok(j),
                        Some(j) if j.status == "failed" || j.status == "cancelled" => return Err(format!("the draft render {}: {}", j.status, j.error.unwrap_or_default())),
                        _ => {}
                    }
                    if started.elapsed() > std::time::Duration::from_secs(600) {
                        return Err("the draft render did not finish within ten minutes; export_status says where it is".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            };
            // The colour check reads the viewer while the draft renders on
            // decoders of its own. One after the other, its forty seconds
            // came after the render and a review of a two-and-a-half
            // minute piece outran the tool's two minutes (#519).
            let (job, check) = std::thread::scope(|s| {
                let check = s.spawn(|| handle(ctx, ControlCommand::Check { color: true, loudness: true, min_severity: None }, &mut None));
                // The render's own time, not the check's if that ends later.
                let job = rendered().map(|j| (j, started.elapsed().as_secs_f64()));
                (job, check.join().unwrap_or_else(|_| Err("the check stopped unexpectedly".into())))
            });
            let (job, render_secs) = job?;
            let check = check?;
            let frames = frames.unwrap_or(((duration / 2.0).ceil() as u32).clamp(8, 48));
            // The draft is small: its sheet is read on the CPU, as quick
            // as the GPU here and never hung on a seek as that did (#519).
            let sheet = klipp_analysis::sheet::contact_sheet_on_cpu(std::path::Path::new("ffmpeg"), &draft, 0.0, duration, frames, cols.unwrap_or(6), 320, &sheet_out)?;
            Ok(json!({
                "draft": draft,
                "sheet": sheet,
                "render_secs": render_secs,
                "review_secs": started.elapsed().as_secs_f64(),
                "loudness": job.loudness,
                "check": check,
                "text": format!("Draft rendered in {render_secs:.1} s at {w}x{h}, reviewed in {:.1} s; {frames} frames on the sheet at {}. {}", started.elapsed().as_secs_f64(), sheet_out.display(), check["text"].as_str().unwrap_or("")),
                "next": "Read the sheet for the shape of the cut, and the check for what to fix; frame_at for any frame that needs a closer look.",
            }))
        }

        ControlCommand::Narrate { text, at_secs, voice, speed, pause_secs, fit_secs, variation, download_models, apply, path } => {
            if in_batch {
                return Err("narrate synthesises and imports; send it on its own".into());
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err("narrate needs the words to say".into());
            }
            let t = std::time::Instant::now();
            let dir = klipp_audio_search::download_dir().join("narration");
            let out = klipp_audio::narration::synthesize(&klipp_audio::narration::Request {
                text: &text,
                voice: voice.as_deref(),
                speed,
                pause_secs,
                fit_secs,
                variation,
                download: download_models,
                path: path.map(std::path::PathBuf::from),
                dir: &dir,
            })?;
            let mut v = handle(ctx, ControlCommand::import_one(out.to_string_lossy().to_string(), true), &mut None)?;
            let media_id = imported_id(ctx, &out).ok_or("the narration was made but did not reach the media pool")?;
            v["media_id"] = json!(media_id);
            v["path"] = json!(out);
            v["synth_secs"] = json!(t.elapsed().as_secs_f64());
            // How long it lasts, said: a fitted take within a frame of its target.
            if let Ok(secs) = klipp_audio::narration::wav_secs(&out) {
                v["duration_secs"] = json!(secs);
            }
            if apply {
                let at = at_secs.unwrap_or(0.0).max(0.0);
                let ops = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    let media_id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media id: {e}"))?);
                    klipp_document::ops::place_narration(&seq, media_id, at)
                };
                let (plan, revision) = resolve_plan(ctx, &ops)?;
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let placed = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Narrate".into()))?;
                v["placed"] = placed;
                v["text"] = json!(format!("Narration made and placed at {at:.2} s on the Voice track as dialogue: {}", plan.text()));
                v["next"] = json!("Duck the music under it (duck), and transcript reads the words back with their times for cutting to them.");
            } else {
                v["text"] = json!(format!("Narration made and imported as {media_id}: {}", out.display()));
                v["next"] = json!("Place it (place on an audio track, role dialogue), or run narrate again with apply true and at_secs.");
            }
            Ok(v)
        }

        ControlCommand::Record { secs, target, fps, path, import, wait, action, recording_id, codec, quality } => {
            use super::recording::{captured, recorder_args, Capture, Recorder, Recording, State};
            if in_batch {
                return Err("record takes the time it records; send it on its own".into());
            }
            // The finished file, imported unless asked not to, with what
            // was captured: the reply both ways of recording end in.
            let finish = |rec: &Recording| -> Result<serde_json::Value, String> {
                let stopped = rec.stop();
                // Given once, then gone from the registry.
                Recording::forget(&rec.id);
                let how = stopped?;
                let out = &rec.path;
                let secs = rec.started.elapsed().as_secs_f64().min(rec.max_secs);
                let mut v = json!({ "recording_id": rec.id, "state": "finished", "stopped": how, "path": out, "secs": secs, "target": rec.capture.target,
                    "asked": { "recorder": rec.recorder.name(), "codec": rec.capture.codec, "quality": rec.capture.quality, "fps": rec.capture.fps } });
                if let Some(c) = captured(out) {
                    v["captured"] = c;
                }
                if import.unwrap_or(true) {
                    if imported_id(ctx, out).is_none() {
                        handle(ctx, ControlCommand::import_one(out.to_string_lossy().to_string(), true), &mut None)?;
                    }
                    v["media_id"] = json!(imported_id(ctx, out));
                    v["next"] = json!("Recorded and imported; analyze reads it (the beat sheet finds the reveals), then place or plan with it.");
                } else {
                    v["next"] = json!("Recorded. import_media brings it into the pool.");
                }
                let size = v["captured"].as_object().map_or(String::new(), |c| format!(", {}x{} {} at {} fps", c["width"], c["height"], c["codec"].as_str().unwrap_or("?"), c["fps"]));
                v["text"] = json!(format!("Recorded {secs:.1} s of {} to {}{size}", rec.capture.target, out.display()));
                Ok(v)
            };
            match action.as_deref().unwrap_or("start") {
                "start" => {}
                which @ ("status" | "stop") => {
                    let id = recording_id.as_deref().ok_or_else(|| format!("record {which} needs the recording_id start gave"))?;
                    let rec = Recording::find(id).ok_or_else(|| format!("no recording {id}; start one with record, wait false"))?;
                    if which == "stop" {
                        return finish(&rec);
                    }
                    // A status only reads: one that reached its length is
                    // `ended` until a stop finishes and imports it.
                    let bytes = rec.bytes();
                    let state = rec.state();
                    return Ok(json!({ "recording_id": rec.id, "state": state.label(), "ready": state == State::Recording,
                        "elapsed_secs": rec.started.elapsed().as_secs_f64(), "max_secs": rec.max_secs, "bytes": bytes, "path": rec.path,
                        "text": format!("{} {}: {:.1} s of at most {:.0}, {bytes} bytes written", rec.id, state.label(), rec.started.elapsed().as_secs_f64(), rec.max_secs),
                        "next": match state {
                            State::Recording => "Recording. Do the live action, then record with action stop.",
                            State::Ended => "It reached its length and stopped: record with action stop finishes the file and imports it.",
                            State::Stopping => "Being stopped: ask again in a moment.",
                            _ => "Not writing yet: ask again in a moment; begin once ready is true.",
                        } }));
                }
                other => return Err(format!("record action is start, status or stop, not {other:?}")),
            }
            let secs = secs.ok_or("record needs secs: how long to record (with wait false, the most)")?;
            if !secs.is_finite() || !(0.5..=600.0).contains(&secs) {
                return Err("secs is how long to record, half a second to ten minutes".into());
            }
            // Its own id, and its own file: two in one second don't share.
            let id = format!("rec-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
            let out = match path {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    let dir = klipp_media::made::made_dir("recordings");
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    dir.join(format!("klipp-recording-{}-{}.mp4", chrono_stamp(), &id[4..]))
                }
            };
            let capture = Capture { target: target.unwrap_or_else(|| "focused".into()), fps: fps.unwrap_or(60).clamp(1, 240), codec: codec.unwrap_or_else(|| "h264".into()), quality };
            let (program, recorder) = if let Some(gsr) = which("gpu-screen-recorder") {
                (gsr, Recorder::GpuScreenRecorder)
            } else if let Some(wfr) = which("wf-recorder") {
                (wfr, Recorder::WfRecorder)
            } else {
                return Err("no screen recorder found: install gpu-screen-recorder (or wf-recorder) and it is used".into());
            };
            let args = recorder_args(recorder, &capture, &out)?;
            let rec = Recording::start(id, &program, &args, out, secs, capture, recorder)?;
            if wait.unwrap_or(true) {
                std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                return finish(&rec);
            }
            Ok(json!({ "recording_id": rec.id, "state": "starting", "path": rec.path, "max_secs": secs,
                "text": format!("Recording {} as {}, for at most {secs:.0} s", rec.capture.target, rec.id),
                "next": "record with action status and this recording_id until ready is true, do the live action, then record with action stop." }))
        }

        ControlCommand::WindowScreenshot { path, max_width } => {
            if in_batch {
                return Err("window_screenshot is not part of a batch; send it on its own".into());
            }
            let path = match path {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::temp_dir().join(format!("klipp-window-{}.png", std::process::id())),
            };
            if !path.is_absolute() {
                return Err("path must be absolute".into());
            }
            let _ = std::fs::remove_file(&path);
            send(AppCommand::CaptureWindow { path: path.clone(), max_width: Some(max_width.unwrap_or(1600)) })?;
            // Written by the UI on the frame after it asks egui; waited for
            // until it is there and whole.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some((width, height)) = std::fs::read(&path).ok().as_deref().and_then(png_size_if_whole) {
                    return Ok(json!({
                        "path": path.to_string_lossy(),
                        "width": width,
                        "height": height,
                        "note": "The window as drawn: the viewer through its monitor LUT and overlays. Measure colour with screenshot or frame_stats, not this.",
                    }));
                }
                if std::time::Instant::now() >= deadline {
                    return Err("the window was not captured within 5 s (is the window shown?)".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        ControlCommand::Screenshot { path, max_width, min_generation, timeout_ms } => {
            if in_batch {
                return read_in_batch("screenshot");
            }
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
            let playing = ctx.shared.transport.lock().map(|t| t.playing).unwrap_or(false);
            // Where on the timeline a playing capture is, when the engine said.
            let mut from_gpu: Option<Option<klipp_core::RationalTime>> = None;
            let (data, width, height, generation) = loop {
                // While playing, the frame on screen is only in the
                // viewer's GPU texture, and the bytes here are an older
                // frame: a paused one, or a decode without its titles or
                // rotation (#544). Read the texture back then, outside the
                // lock. A generation counts frames of bytes, which playing
                // writes none of, so only a paused capture waits for one.
                let gpu = {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    (min_generation.is_none() || playing).then(|| fb.gpu_output_snapshot()).flatten()
                };
                if let Some(mut snapshot) = gpu {
                    // The engine submits a frame's drawing, then names it
                    // under the lock. A copy queued in between reads the
                    // new frame under the old one's name, so a frame
                    // counted while reading means read again (#564).
                    let mut data = snapshot.read()?;
                    for _ in 0..4 {
                        let newer = {
                            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                            fb.gpu_output_snapshot().filter(|s| s.frame != snapshot.frame)
                        };
                        let Some(next) = newer else { break };
                        snapshot = next;
                        data = snapshot.read()?;
                    }
                    let generation = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_generation();
                    from_gpu = Some(snapshot.presented);
                    break (data, snapshot.width, snapshot.height, generation);
                }
                {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    let (d, w, h, g) = fb.latest_frame();
                    // The UI takes the bytes out of the buffer while it
                    // uploads them and puts them back after; in between
                    // the frame is there but its bytes are not. Wait for
                    // them like a frame that has not arrived.
                    if min_generation.is_none_or(|m| g >= m) && !d.is_empty() {
                        break (d.to_vec(), w, h, g);
                    }
                    if std::time::Instant::now() >= deadline {
                        if d.is_empty() {
                            return Err(format!(
                                "the viewer has no frame bytes within {} ms (generation {g}): \
                                 while playing, frames stay on the GPU; pause first.",
                                timeout_ms.unwrap_or(3000),
                            ));
                        }
                        // Say so rather than hand back the old frame as if
                        // it were the new one. A timeout here is itself a
                        // finding: the change did not trigger a re-render.
                        return Err(format!(
                            "no frame with generation >= {} within {} ms (newest is {g}). \
                             The command was applied but did not trigger a re-render.",
                            min_generation.unwrap_or(0),
                            timeout_ms.unwrap_or(3000),
                        ));
                    }
                }
                // Lock released while waiting so the engine can write.
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            if width == 0 || height == 0 {
                return Err("viewer has no frame yet".into());
            }
            let expected = (width as usize) * (height as usize) * 4;
            if data.len() < expected {
                return Err(format!(
                    "viewer buffer is {} bytes, short of {width}x{height}x4 = {expected}",
                    data.len(),
                ));
            }
            // Downscale before encoding, not after: the point is to
            // avoid writing (and then shipping) 33 MB for a frame someone
            // only wants to look at.
            let (data, width, height) = match max_width {
                Some(mw) if mw > 0 && mw < width => {
                    downscale_rgba(&data[..expected], width, height, mw)
                }
                _ => (data[..expected].to_vec(), width, height),
            };
            // A relative path would resolve against *Klipp's* working
            // directory, not the caller's — which is how a batch of test
            // screenshots once landed in the repo root. Refuse it.
            if let Some(p) = path.as_deref() {
                if !std::path::Path::new(p).is_absolute() {
                    return Err(format!(
                        "screenshot path must be absolute (got {p:?}); a relative path \
                         would resolve against Klipp's working directory"
                    ));
                }
            }
            let out = path.map(std::path::PathBuf::from).unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "klipp-screenshot-{}.png",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0),
                ))
            });
            write_png(&out, &data, width, height)?;
            let mut v = json!({
                "path": out.to_string_lossy(),
                "width": width,
                "height": height,
                "generation": generation,
            });
            if let Some(at) = from_gpu {
                v["playing_frame"] = json!(true);
                // Which moment the capture shows: the playhead has moved on
                // by the time the reply arrives.
                if let Some(at) = at {
                    let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps.as_f64();
                    let secs = at.to_secs_f64();
                    v["secs"] = json!(secs);
                    v["frame"] = json!((secs * fps).round() as i64);
                }
            }
            // The capture is always the whole graded frame. When the viewer
            // is showing a compare split, what the person sees is not that,
            // and a reader comparing notes with them needs to know.
            if let Some(compare) = ctx.shared.viewer_compare.lock().ok().and_then(|c| c.clone()) {
                v["viewer_compare"] = json!(compare);
                v["note"] = json!("A compare split is on in the viewer; this image is the whole graded frame, not the split the viewer shows.");
            }
            Ok(v)
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}
