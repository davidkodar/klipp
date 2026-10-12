//! Output requests: exports and subtitles, saving and opening, frame statistics, sheets, frames, performance and the log (#826).

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
        ControlCommand::Export { path, width, height, fps, crf, bit_depth, target_lufs, true_peak_dbtp, burn_captions, preset, codec, audio_codec, bitrate_kbps } => {
            let out = std::path::PathBuf::from(&path);
            if !out.is_absolute() {
                return Err(format!("export path must be absolute (got {path:?})"));
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            // Nothing to export: it rendered a 261-byte file and called it a
            // success (#655).
            if seq.has_nothing_to_export() {
                return Err("the timeline is empty: there is nothing to export. Place some clips first.".into());
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let offline: Vec<String> = seq
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter_map(|c| c.media_id().and_then(|id| pool.get(&id)))
                .filter(|m| m.offline)
                .map(|m| m.filename.clone())
                .collect();
            if !offline.is_empty() {
                return Err(format!("media offline, export refused: {}", offline.join(", ")));
            }
            if pool.all().any(|m| m.path == out) {
                return Err(format!("{path} is a media file of this project; an export cannot overwrite its own source"));
            }
            // What the export does not say for itself, the brief says.
            let brief = seq.brief.clone();
            // The credits of what this sequence uses, and nothing else the
            // shared download library holds (#1582).
            let credits = sequence_credits(&seq, &pool);
            drop(pool);
            drop(seq);
            // Size: as asked, else the preset's, else the brief's.
            let (width, height) = match (width, height, brief.as_ref()) {
                (None, None, Some(b)) if preset.is_none() => (b.width, b.height),
                _ => (width, height),
            };
            let brief_lufs = brief.as_ref().and_then(|b| b.loudness_lufs);
            let format = match out.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
                Some("mp4") | None => klipp_core::ExportFormat::Mp4,
                Some("mov") => klipp_core::ExportFormat::Mov,
                Some("mkv") => klipp_core::ExportFormat::Mkv,
                Some("webm") => klipp_core::ExportFormat::WebM,
                Some(other) => return Err(format!("unsupported container .{other}; use mp4, mov, mkv or webm")),
            };
            let mut settings = klipp_core::ExportSettings { output_path: out.clone(), format, ..Default::default() };
            // The default H.264 and AAC do not go in WebM.
            settings.fit_codecs_to_format();
            // A preset first, so every field given beside it wins (#1583).
            let preset_name = match preset.as_deref() {
                Some(name) => Some(apply_export_preset(&mut settings, name)?),
                None => None,
            };
            if let (Some(w), Some(h)) = (width, height) {
                let most = klipp_core::ExportSettings::MAX_SIDE;
                if w > most || h > most || w < 2 || h < 2 {
                    return Err(format!("{w}x{h} is not a size to export at; give each side between 2 and {most}"));
                }
                settings.resolution = Some(klipp_core::Resolution { width: w, height: h });
            }
            if let Some(f) = fps {
                settings.fps = Some(fps_from_f64(f));
            }
            if let Some(c) = crf {
                settings.crf = c;
            }
            match bit_depth {
                None => {}
                Some(d @ (8 | 10)) => settings.bit_depth = d,
                Some(d) => return Err(format!("bit_depth {d} is not a depth; give 8 or 10")),
            }
            if let Some(name) = codec.as_deref() {
                settings.video_codec = video_codec_named(name)?;
            }
            if let Some(name) = audio_codec.as_deref() {
                settings.audio_codec = audio_codec_named(name)?;
            }
            use klipp_core::ExportVideoCodec as V;
            let fixed_rate = matches!(settings.video_codec, V::ProRes422 | V::ProRes422HQ | V::ProRes4444 | V::DNxHD115 | V::DNxHD145);
            if let Some(kbps) = bitrate_kbps {
                if crf.is_some() {
                    return Err("give crf (a quality) or bitrate_kbps (a rate), not both".into());
                }
                if fixed_rate {
                    return Err(format!("{} has a rate of its own; leave bitrate_kbps out", settings.video_codec.label()));
                }
                if !(100..=500_000).contains(&kbps) {
                    return Err(format!("{kbps} kbps is not a video bitrate; give 100 to 500000"));
                }
                settings.rate_control = klipp_core::RateControl::Vbr;
                settings.video_bitrate_kbps = Some(kbps);
            }
            if settings.bit_depth == 10 && !matches!(settings.video_codec, V::H264 | V::H265 | V::ProRes422 | V::ProRes422HQ | V::ProRes4444) {
                return Err(format!("{} is written at 8 bits; bit_depth 10 needs h264, hevc or ProRes", settings.video_codec.label()));
            }
            if let Some(problem) = settings.container_problem() {
                return Err(problem);
            }
            // Loudness: as asked, else the preset's, else the brief's.
            match target_lufs {
                Some(l) => settings.target_lufs = Some(l),
                None if preset_name.is_none() && brief_lufs.is_some() => settings.target_lufs = brief_lufs,
                None => {}
            }
            if let Some(ceiling) = true_peak_dbtp {
                if !ceiling.is_finite() || !(-10.0..=0.0).contains(&ceiling) {
                    return Err(format!("a true-peak ceiling of {ceiling} dBTP: give one from -10 to 0, -2 for ATSC A/85"));
                }
                settings.true_peak_dbtp = Some(ceiling);
            }
            if let Some(burn) = burn_captions {
                settings.burn_in_subtitles = burn;
            }
            // Said, because it cannot be seen from the reply otherwise and
            // decides whether a sidecar file is needed.
            let said = json!({
                "captions_and_titles_burnt_in": settings.burn_in_subtitles,
                "target_lufs": settings.target_lufs,
                "true_peak_dbtp": settings.target_lufs.map(|_| settings.true_peak_dbtp.unwrap_or(-1.0)),
                // The size it is encoded at: an odd side is rounded down,
                // as 4:2:0 video needs (hunt #2), and said so.
                "size": settings.resolution.map(|r| {
                    let (w, h) = settings.output_size(r.width, r.height);
                    if (w, h) == (r.width, r.height) { format!("{w}x{h}") } else { format!("{w}x{h} (rounded down to even from {}x{})", r.width, r.height) }
                }),
                // Chosen by the container (VP9 and Opus for .webm, a slower
                // software encode that is always 8-bit), so said.
                "video_codec": settings.video_codec.label(),
                "audio_codec": settings.audio_codec.label(),
                "preset": preset_name,
                "rate": match (settings.rate_control, settings.video_bitrate_kbps) {
                    (klipp_core::RateControl::Crf, _) | (_, None) => format!("crf {}", settings.crf),
                    (_, Some(kbps)) => format!("{kbps} kbps"),
                },
                "bit_depth": if settings.bit_depth == 10 { 10 } else { 8 },
            });
            let burnt = said["captions_and_titles_burnt_in"].as_bool().unwrap_or(true);
            let known_jobs: std::collections::HashSet<String> = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner())
                .iter().map(|job| job.job_id.clone()).collect();
            let mut v = send(AppCommand::ExportSequence { settings })?;
            v["export"] = said;
            if !credits.is_empty() {
                let file = credits_path(&out);
                match std::fs::write(&file, klipp_audio_search::credits_text(&credits)) {
                    Ok(()) => v["credits"] = json!({ "path": file, "count": credits.len() }),
                    Err(e) => v["credits"] = json!({ "error": format!("the credits list could not be written to {}: {e}", file.display()) }),
                }
            }
            if v["applied"] != true {
                v["next"] = json!("Export submission is still queued for the UI. Resolve its operation_id with operation_status before looking for a render job.");
                return Ok(v);
            }
            let started = std::time::Instant::now();
            let job_id = loop {
                let found = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner()).iter()
                    .find(|job| job.path == out && !known_jobs.contains(&job.job_id))
                    .map(|job| job.job_id.clone());
                if found.is_some() || started.elapsed() >= std::time::Duration::from_millis(300) { break found; }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            if let Some(job_id) = job_id {
                v["job_id"] = json!(job_id);
                v["next"] = json!(format!("Render submitted. follow_job {job_id} waits until it ends and says how; only completed means the file is ready. To tell the person its progress as it goes, run `klipp-mcp follow {job_id}` as a background task: a line per step, the last when it ends.{}",
                    if burnt { "" } else { " Captions are not burnt in: write export_subtitles for a sidecar SRT." }));
            } else {
                v["next"] = json!("The UI acknowledged export, but no render job appeared. It may be awaiting a confirmation dialog or the snapshot may have failed. Read status and export_status before reporting a render as started.");
            }
            Ok(v)
        }

        ControlCommand::ExportStatus { wait, job_id, timeout_ms } => {
            if in_batch {
                return read_in_batch("export_status");
            }
            let ids: Vec<String> = match job_id {
                Some(id) => vec![id],
                None => ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|j| j.job_id.clone()).collect(),
            };
            if wait && ids.is_empty() {
                return Err("no export jobs are known yet; wait for export submission before waiting for completion".into());
            }
            let (jobs, finished) = wait_until(wait, timeout_ms, || {
                let exports = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner());
                let jobs: Vec<_> = ids.iter().map(|id| exports.iter().find(|j| &j.job_id == id).cloned().ok_or_else(|| format!("no export job {id}"))).collect::<Result<_, _>>()?;
                let finished = jobs.iter().all(|j| matches!(j.status.as_str(), "complete" | "failed" | "cancelled"));
                Ok((jobs, finished))
            })?;
            let succeeded = finished && jobs.iter().all(|j| j.status == "complete");
            Ok(json!({ "jobs": jobs, "finished": finished, "succeeded": succeeded, "timed_out": wait && !finished }))
        }

        ControlCommand::ExportSubtitles { path } => {
            let p = std::path::PathBuf::from(&path);
            if !p.is_absolute() {
                return Err(format!("subtitle path must be absolute (got {path:?})"));
            }
            send(AppCommand::ExportSubtitles { path: p })
        }

        ControlCommand::SaveProject { path } => {
            if in_batch {
                return Err("save_project is not part of a batch; send it on its own".into());
            }
            let before = project_status(ctx)?;
            let target = match path {
                Some(p) => {
                    let p = std::path::PathBuf::from(&p);
                    if !p.is_absolute() {
                        return Err(format!("project path must be absolute (got {p:?})"));
                    }
                    with_klipp_extension(p)
                }
                None => before.path.clone().ok_or("the project has never been saved: give a path, a folder ending in .klipp")?,
            };
            send_envelope(ctx, vec![AppCommand::SaveProjectAs { path: target.clone() }], false)?;
            let after = wait_for_project_event(ctx, before.generation, 30_000)?;
            if let Some(e) = after.last_error {
                return Err(e);
            }
            Ok(json!({ "saved": true, "path": target.to_string_lossy(), "revision": ctx.shared.revision() }))
        }

        ControlCommand::OpenProject { path, discard_changes } => {
            if in_batch {
                return Err("open_project is not part of a batch; send it on its own".into());
            }
            let p = std::path::PathBuf::from(&path);
            if !p.is_absolute() {
                return Err(format!("project path must be absolute (got {path:?})"));
            }
            if !p.exists() {
                return Err(format!("no project at {path:?}"));
            }
            let before = project_status(ctx)?;
            if before.dirty && !discard_changes {
                return Err("the open project has unsaved changes: save_project first, or pass discard_changes true to lose them".into());
            }
            send_envelope(ctx, vec![AppCommand::OpenProjectNow { path: p.clone() }], false)?;
            let after = wait_for_project_event(ctx, before.generation, 60_000)?;
            if let Some(e) = after.last_error {
                return Err(e);
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
            Ok(json!({
                "opened": true,
                "path": p.to_string_lossy(),
                "sequence": seq.name,
                "tracks": seq.tracks.len(),
                "clips": clips,
                "duration_secs": seq.total_duration().to_secs_f64(),
                "revision": ctx.shared.revision(),
            }))
        }

        ControlCommand::FrameStats { secs, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_stats");
            }
            let (data, width, height) = grab_frame_rgba(ctx, secs, timeout_ms)?;
            let mut v = frame_stats(&data, width, height, secs);
            let faces = skin_regions(ctx, secs, &data, width, height);
            let m = klipp_color::measure::measure_frame(&data, width, height, &faces);
            let look = look_for(ctx, secs);
            let findings = klipp_color::measure::judge(&m, &look);
            let mut text = v["text"].as_str().unwrap_or("").to_string();
            text.push_str(&format!("\nLab (D65): L* p1 {:.0} p50 {:.0} p99 {:.0}, contrast {:.0}, chroma mean {:.0}; neutrals a* {:.1} b* {:.1} ({}, {:.0}% of the frame)", m.l_p1, m.l_p50, m.l_p99, m.contrast_range, m.chroma_mean, m.cast_a, m.cast_b, if m.cast() < 3.0 { "neutral".to_string() } else { format!("cast toward {}", klipp_color::measure::cast_name(m.cast_a, m.cast_b)) }, m.neutral_pct));
            for (i, f) in m.faces.iter().enumerate() {
                if f.readable() {
                    text.push_str(&format!("\nface {}: skin hue {:.0}° (band {:.0}–{:.0}), chroma {:.0}, L* {:.0}, {} of {}×{} patches, hue spread {:.0}°{}", i + 1, f.hue, klipp_color::measure::SKIN_HUE_LOW, klipp_color::measure::SKIN_HUE_HIGH, f.chroma, f.median.l, f.patches, f.grid, f.grid, f.hue_iqr, if f.mixed_light { " (two lights)" } else { "" }));
                } else if f.patches > 0 {
                    text.push_str(&format!("\nface {}: too little skin to read ({} of {}×{} patches at {:.0}%,{:.0}% of the frame)", i + 1, f.patches, f.grid, f.grid, (f.region[0] + f.region[2] / 2.0) * 100.0, (f.region[1] + f.region[3] / 2.0) * 100.0));
                }
            }
            text.push_str(&format!("\nAgainst the look \"{}\": {}", look.name, findings.iter().filter(|f| f.verdict != klipp_color::measure::Verdict::Pass).map(|f| format!("{:?} {}", f.verdict, f.what)).collect::<Vec<_>>().join("; ").replace("Fail", "FAIL").replace("Warn", "warn")));
            v["text"] = json!(text);
            v["lab"] = json!(m);
            v["look"] = json!(look);
            v["findings"] = json!(findings);
            Ok(v)
        }

        ControlCommand::TimelineSheet { from_secs, to_secs, frames, cols, tile_width, path, inline } => {
            let inline = inline.unwrap_or(path.is_none());
            if in_batch {
                return read_in_batch("timeline_sheet");
            }
            // Where to look: the middle of each stretch between cuts on the
            // visible picture tracks, so each shot is seen once, or evenly.
            let (tiles, names): (Vec<f64>, Vec<String>) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let end = seq.total_duration().to_secs_f64();
                let (a, b) = (from_secs.unwrap_or(0.0).max(0.0), to_secs.unwrap_or(end).min(end));
                if b <= a {
                    return Err(format!("{a:.2}-{b:.2} s is not a stretch of the timeline (0-{end:.2} s)"));
                }
                let pictures = || seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled);
                let times: Vec<f64> = match frames {
                    Some(n) => {
                        let n = n.clamp(1, 60);
                        (0..n).map(|k| a + (b - a) * (k as f64 + 0.5) / n as f64).collect()
                    }
                    None => {
                        let mut cuts: Vec<f64> = vec![a, b];
                        for c in pictures() {
                            for t in [c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()] {
                                if t > a && t < b {
                                    cuts.push(t);
                                }
                            }
                        }
                        cuts.sort_by(f64::total_cmp);
                        cuts.dedup_by(|x, y| (*x - *y).abs() < 0.05);
                        cuts.windows(2).map(|w| (w[0] + w[1]) / 2.0).take(60).collect()
                    }
                };
                // What is on each tile: the pictures, and titles over them.
                let seen = || seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text) && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled && !c.consumed_as_matte);
                let names = times.iter().map(|t| {
                    seen().filter(|c| c.timeline_in.to_secs_f64() <= *t && c.timeline_out.to_secs_f64() > *t).map(|c| match &c.kind {
                        klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "media".into()),
                        klipp_timeline::ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("").chars().take(24).collect::<String>()),
                        klipp_timeline::ClipKind::Generator(_) => "generator".into(),
                        _ => "clip".into(),
                    }).collect::<Vec<_>>().join(" over ")
                }).collect();
                (times, names)
            };
            let tw = tile_width.unwrap_or(320).clamp(80, 960);
            let cols = cols.unwrap_or(6).clamp(1, 12).min(tiles.len() as u32).max(1);
            let mut grabbed: Vec<(Vec<u8>, u32, u32)> = Vec::new();
            for t in &tiles {
                let (rgba, w, h) = grab_frame_rgba(ctx, *t, Some(8000))?;
                grabbed.push(downscale_rgba(&rgba, w, h, tw));
            }
            let th = grabbed.iter().map(|g| g.2).max().unwrap_or(1);
            let rows = (grabbed.len() as u32).div_ceil(cols);
            let (sw, sh) = (cols * tw, rows * th);
            let mut sheet = vec![0u8; (sw * sh * 4) as usize];
            for (k, (px, w, h)) in grabbed.iter().enumerate() {
                let (x0, y0) = ((k as u32 % cols) * tw, (k as u32 / cols) * th);
                for y in 0..*h {
                    let src = &px[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
                    let at = (((y0 + y) * sw + x0) * 4) as usize;
                    sheet[at..at + src.len()].copy_from_slice(src);
                }
            }
            let out = path.map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("klipp-timeline-sheet-{}.png", uuid::Uuid::new_v4())));
            write_png(&out, &sheet, sw, sh)?;
            let listed: Vec<serde_json::Value> = tiles.iter().zip(&names).enumerate().map(|(k, (t, n))| json!({ "tile": k + 1, "secs": (t * 100.0).round() / 100.0, "shows": n })).collect();
            let text = format!(
                "{} tiles, {cols} to a row, left to right then down:\n{}",
                tiles.len(),
                tiles.iter().zip(&names).enumerate().map(|(k, (t, n))| format!("  {:>2}. {t:.2} s  {n}", k + 1)).collect::<Vec<_>>().join("\n"),
            );
            Ok(json!({ "text": text, "path": out.to_string_lossy(), "width": sw, "height": sh, "tiles": listed, "inline": inline }))
        }

        ControlCommand::FrameAt { secs, path, max_width, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_at");
            }
            // One at a time: there is one playhead, and two grabs at once
            // seek it out from under each other and the second fails.
            static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
            let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
            let target = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
            // A picture there whose file is missing renders nothing to
            // wait for: said at once, by name, not after the whole timeout
            // as "the viewer is at 0.000 s" (#657).
            {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                if let Some(missing) = offline_picture_at(&seq, &pool, target) {
                    return Err(format!("no frame at {secs:.3} s: {missing}; relink it (relink_media) or look at another time"));
                }
            }
            let held = hold_transport(ctx)?;
            let before = {
                let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                fb.latest_generation()
            };
            // Paused first: a frame of a time cannot be taken from a viewer
            // that is moving, and a grab during playback used to wait out
            // its whole timeout while the playhead ran on past the target.
            send_envelope(ctx, vec![AppCommand::Pause, AppCommand::SeekTo(target)], false)?;
            // The frame this asks for: rendered after the seek, at the
            // time asked for, with every clip under the playhead in it.
            // A frame's generation alone said none of that: a late
            // re-render of the previous position passed as this one, and
            // a frame shown before the other tracks' decoders had their
            // pictures — the engine fills those in when they arrive — was
            // handed back as the record of the timeline.
            let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(FRAME_READ_TIMEOUT_MS));
            let deadline = std::time::Instant::now() + wait;
            let generation = loop {
                let (g, at, complete) = {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    (fb.latest_generation(), fb.latest_presented(), fb.latest_complete())
                };
                let at_target = at.is_some_and(|a| a.cmp(&target) == std::cmp::Ordering::Equal);
                if g != before && at_target && complete {
                    break g;
                }
                if std::time::Instant::now() >= deadline {
                    restore_transport(ctx, held);
                    let state = match (at_target, complete) {
                        (false, _) => format!(
                            "the viewer is at {} s, not {secs:.3} s",
                            at.map(|a| format!("{:.3}", a.to_secs_f64())).unwrap_or_else(|| "nowhere".into())
                        ),
                        (true, false) => "the frame is there but the other tracks' clips are still decoding".to_string(),
                        (true, true) => "no frame was rendered after the seek".to_string(),
                    };
                    return Err(format!("no frame at {secs:.3} s within {} ms: {state}; give a longer timeout_ms", wait.as_millis()));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            let shot = handle(
                ctx,
                ControlCommand::Screenshot { path, max_width, min_generation: Some(generation), timeout_ms },
                &mut None,
            );
            // Back where the playhead was, whatever the capture did.
            restore_transport(ctx, held);
            let mut v = shot?;
            v["secs"] = json!(secs);
            v["frame"] = json!((secs * fps.as_f64()).round() as i64);
            Ok(v)
        }

        ControlCommand::Perf => {
            if in_batch {
                return read_in_batch("perf");
            }
            // Measured over the last two seconds of published pictures, so
            // a reader sees the rate played, not only the rate asked for.
            let presented_fps = ctx
                .frame_buffer
                .lock()
                .map_err(|_| "frame buffer lock poisoned")?
                .presented_fps(std::time::Instant::now(), std::time::Duration::from_secs(2));
            let ring = ctx.perf_ring.lock().map_err(|_| "perf ring lock poisoned")?;
            let latest = ring.latest().cloned();
            let avg = ring.averages(60);
            Ok(json!({
                "presented_fps": presented_fps,
                "latest": latest.map(|p| json!({
                    "width": p.width,
                    "height": p.height,
                    "target_fps": p.target_fps,
                    "decode_ms": p.decode_ms,
                    "grade_ms": p.grade_ms,
                    "composite_ms": p.composite_ms,
                    "total_ms": p.total_ms,
                    "dropped": p.dropped,
                })),
                "average_of_last_60": {
                    "decode_ms": avg.decode_ms,
                    "grade_ms": avg.grade_ms,
                    "total_ms": avg.total_ms,
                },
                "readbacks": {
                    "count": klipp_engine::CPU_READBACK_COUNT
                        .load(std::sync::atomic::Ordering::Relaxed),
                    "total_us": klipp_engine::CPU_READBACK_US
                        .load(std::sync::atomic::Ordering::Relaxed),
                },
                // Clip renders whose effects ran in the grade's plan, read
                // from the clip's picture, and those that fell back to the
                // list (M2 step 10c); cumulative.
                "plan_effects": {
                    "clips": klipp_compose::PLAN_EFFECT_CLIPS.load(std::sync::atomic::Ordering::Relaxed),
                    "fallback_lists": klipp_compose::PLAN_EFFECT_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed),
                    // Effect runs a plan couldn't apply, drawn by the CPU
                    // kernels instead (M2 step 11d); zero unless one failed.
                    "on_cpu": klipp_compose::EFFECTS_ON_CPU.load(std::sync::atomic::Ordering::Relaxed),
                    // Clip effect runs the CPU kernels drew, for any reason,
                    // failures included (#1274); zero on a GPU machine
                    // unless an effect has no GPU kernel.
                    "on_cpu_kernels": klipp_compose::EFFECTS_ON_CPU_KERNELS.load(std::sync::atomic::Ordering::Relaxed),
                    // Masked effects whose matte their run couldn't give,
                    // drawn bypassed (M2 step 9c-2); zero unless one was.
                    "mask_unavailable": klipp_compose::EFFECT_MASKS_UNAVAILABLE.load(std::sync::atomic::Ordering::Relaxed),
                    // Frames the GPU stack declined to the CPU composite,
                    // whatever the reason; cumulative.
                    "stack_declines": klipp_compose::GPU_STACK_DECLINES.load(std::sync::atomic::Ordering::Relaxed),
                    // Masked effects in a run apart from their grade, their
                    // matte made apart and blended on the CPU (9c-2 B2).
                    "blended_on_cpu": klipp_compose::EFFECTS_BLENDED_ON_CPU.load(std::sync::atomic::Ordering::Relaxed),
                    // Clip pictures lifted cold (none while playing with no
                    // edit: a lift is cached until the clip changes).
                    "cold_lifts": klipp_timeline::COLD_LIFTS.load(std::sync::atomic::Ordering::Relaxed),
                },
            }))
        }

        ControlCommand::LogTail { lines } => {
            let n = lines.unwrap_or(100).clamp(1, 5000);
            let text = std::fs::read_to_string(&ctx.log_path)
                .map_err(|e| format!("reading {}: {e}", ctx.log_path.display()))?;
            let tail: Vec<&str> = text.lines().rev().take(n).collect();
            Ok(json!({
                "path": ctx.log_path.to_string_lossy(),
                "lines": tail.into_iter().rev().collect::<Vec<_>>(),
            }))
        }

        _ => Err("internal: a request was routed to the wrong area".into()),
    }
}

/// The built-in export preset `name` (its name, case aside, or its id)
/// applied to `settings`: its codecs, size, rate, quality and loudness.
/// The container comes from the path, so it must be the preset's
/// (#1583). Returns the preset's name, for the reply.
fn apply_export_preset(settings: &mut klipp_core::ExportSettings, name: &str) -> Result<String, String> {
    let presets = klipp_core::builtin_presets();
    let wanted = name.trim().to_lowercase();
    let Some(p) = presets.iter().find(|p| p.name.to_lowercase() == wanted || p.id == name.trim()) else {
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        return Err(format!("no export preset {name:?}; the presets are {}", names.join(", ")));
    };
    let s = &p.settings;
    if s.format != settings.format {
        return Err(format!("the preset {:?} writes .{}; give a path ending .{}", p.name, s.format.extension(), s.format.extension()));
    }
    settings.video_codec = s.video_codec;
    settings.resolution = s.resolution;
    settings.fps = s.fps;
    settings.crf = s.crf;
    settings.preset = s.preset;
    settings.audio_codec = s.audio_codec;
    settings.audio_bitrate_kbps = s.audio_bitrate_kbps;
    settings.video_bitrate_kbps = s.video_bitrate_kbps;
    settings.rate_control = s.rate_control;
    settings.target_lufs = s.target_lufs;
    settings.true_peak_dbtp = s.true_peak_dbtp;
    Ok(p.name.clone())
}

/// A video codec by the name an agent gives it (#1583).
fn video_codec_named(name: &str) -> Result<klipp_core::ExportVideoCodec, String> {
    use klipp_core::ExportVideoCodec as V;
    Ok(match name.trim().to_lowercase().replace(['.', ' ', '-'], "_").as_str() {
        "h264" | "h_264" | "avc" => V::H264,
        "hevc" | "h265" | "h_265" => V::H265,
        "prores" | "prores_422" => V::ProRes422,
        "prores_hq" | "prores_422_hq" => V::ProRes422HQ,
        "prores_4444" => V::ProRes4444,
        "dnxhd" | "dnxhd_115" => V::DNxHD115,
        "dnxhd_145" => V::DNxHD145,
        "av1" => V::Av1,
        "vp9" => V::Vp9,
        _ => return Err(format!("{name:?} is not a video codec; give h264, hevc, prores, prores_hq, prores_4444, dnxhd_115, dnxhd_145, av1 or vp9")),
    })
}

/// An audio codec by the name an agent gives it (#1583).
fn audio_codec_named(name: &str) -> Result<klipp_core::ExportAudioCodec, String> {
    use klipp_core::ExportAudioCodec as A;
    Ok(match name.trim().to_lowercase().as_str() {
        "aac" => A::Aac,
        "mp3" => A::Mp3,
        "flac" => A::Flac,
        "opus" => A::Opus,
        "pcm" | "pcm16" | "wav" => A::Pcm16,
        "pcm24" => A::Pcm24,
        _ => return Err(format!("{name:?} is not an audio codec; give aac, mp3, flac, opus, pcm16 or pcm24")),
    })
}

/// The credits list's file, beside the export: `film.mp4` gets
/// `film.credits.txt`.
fn credits_path(out: &std::path::Path) -> std::path::PathBuf {
    out.with_extension("credits.txt")
}

/// The credit of each downloaded file the sequence's enabled clips use,
/// each file once: what its export owes credit for. A file with no credit
/// beside it (the person's own footage) has none.
fn sequence_credits(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<klipp_audio_search::Credit> {
    let mut seen = std::collections::HashSet::new();
    seq.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled)
        .filter_map(|c| c.media_id().and_then(|id| pool.get(&id)))
        .filter(|m| seen.insert(m.path.clone()))
        .filter_map(|m| klipp_audio_search::read_credit(&m.path))
        .collect()
}

#[cfg(test)]
mod credits_tests {
    use super::{credits_path, sequence_credits};

    /// #1582: the credits are those of the files the sequence uses, each
    /// once, not every download in the shared library; a disabled clip
    /// and footage with no credit add nothing.
    #[test]
    fn the_credits_are_the_sequences_own() {
        let dir = std::env::temp_dir().join(format!("klipp-sequence-credits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, credited: bool| {
            let audio = dir.join(format!("{name}.mp3"));
            std::fs::write(&audio, b"x").unwrap();
            if credited {
                let page = format!("https://freesound.org/people/a/sounds/{name}/");
                std::fs::write(audio.with_extension("klipp-credit.txt"), format!("\"{name}\" by a — CC BY ({page})\nLicence: CC BY (https://creativecommons.org/licenses/by/4.0/)\nSource page: {page}\n")).unwrap();
            }
            audio
        };
        let mut pool = klipp_media::MediaPool::new();
        let mut add = |name: &str, credited: bool| pool.add(klipp_media::MediaItem::synthetic_for_test(file(name, credited), None));
        let (whoosh, riser, unused, footage, off) = (add("whoosh", true), add("riser", true), add("unused", true), add("footage", false), add("off", true));
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let clip = |id, at: f64| klipp_timeline::Clip::media(id, tc(at), tc(at + 1.0), tc(0.0), tc(1.0));
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut picture = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        picture.clips = vec![clip(footage, 0.0)];
        let mut sound = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        let mut disabled = clip(off, 4.0);
        disabled.enabled = false;
        sound.clips = vec![clip(whoosh, 0.0), clip(riser, 1.0), clip(whoosh, 2.0), disabled];
        seq.tracks = vec![picture, sound];
        let _ = unused;

        let mut titles: Vec<String> = sequence_credits(&seq, &pool).into_iter().map(|c| c.title).collect();
        titles.sort();
        assert_eq!(titles, ["riser", "whoosh"]);
        assert_eq!(credits_path(std::path::Path::new("/out/film.mp4")), std::path::Path::new("/out/film.credits.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
