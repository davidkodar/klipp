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
        ControlCommand::DeliverySpecs => {
            use klipp_core::delivery_spec as spec;
            let specs = spec::all_specs();
            let lines: Vec<String> = specs.iter().map(|s| format!("{} ({}){}", s.name, s.id, if s.built_in { "" } else { ", yours" })).collect();
            Ok(json!({
                "text": format!("Delivery specs:\n{}", lines.join("\n")),
                "specs": specs.iter().map(|s| json!({ "id": s.id, "name": s.name, "built_in": s.built_in, "sources": s.sources, "requirements": s.requirements, "export": s.export, "hdr": s.hdr })).collect::<Vec<_>>(),
                "next": "check_delivery with a spec's id checks the timeline, or a finished file with file.",
            }))
        }
        ControlCommand::CheckDelivery { spec, file } => {
            use klipp_core::delivery_spec as ds;
            let spec = ds::find_spec(&spec).ok_or_else(|| format!("no delivery spec {spec:?}; delivery_specs lists them"))?;
            let (facts, what) = match &file {
                Some(path) => (klipp_render::delivery_facts::file_facts(std::path::Path::new(path), true)?, path.clone()),
                None => {
                    if in_batch {
                        return read_in_batch("check_delivery");
                    }
                    let (size, fps, captions) = {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        let captions = seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Subtitle && !t.clips.is_empty());
                        (seq.resolution, seq.fps.as_f64(), captions)
                    };
                    // The mix is read only when the export would not set its
                    // loudness.
                    let mix = if spec.export.target_lufs.is_none() {
                        let snapshot = {
                            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                            klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear)
                                .map_err(|e| format!("the mix could not be read: {e}"))?
                        };
                        crate::grading::mix_delivery(&snapshot).ok()
                    } else {
                        None
                    };
                    (ds::export_facts(&spec.export, spec.hdr, size, fps, captions, mix), "the timeline, exported with the spec's settings".to_string())
                }
            };
            let findings = ds::check(&spec, &facts);
            let verdict = ds::verdict(&findings);
            let word = |s: ds::Status| match s { ds::Status::Pass => "pass", ds::Status::Warn => "warn", ds::Status::Fail => "fail" };
            let lines: Vec<String> = findings.iter().map(|f| format!("{:4}  {}: {} (wanted {}){}", word(f.status), f.what, f.found, f.wanted, if f.fix.is_empty() { String::new() } else { format!(" — {}", f.fix) })).collect();
            Ok(json!({
                "text": format!("{} against {}: {}\n{}", what, spec.name, word(verdict), lines.join("\n")),
                "spec": spec.id,
                "verdict": word(verdict),
                "findings": findings,
                "sources": spec.sources,
            }))
        }
        ControlCommand::Export { path, width, height, fps, crf, bit_depth, target_lufs, true_peak_dbtp, burn_captions, captions_608, hdr10plus, hdr, peak_nits, preset, codec, audio_codec, bitrate_kbps, encoder_speed } => {
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
            // A tracked subject matte short of the clip would render its
            // missing frames with a neighbour's matte (#239).
            let gaps = klipp_compose::subject_matte::uncovered_with(&seq, |clip, model| {
                let item = pool.get(&clip.media_id()?)?;
                klipp_compose::subject_matte::media_dir(&item.path, klipp_compose::subject_matte::decode_rotation(clip, item.source_rotation), model)
            }, |clip| {
                // Past its end, its last frame is read (#985 phase 3).
                let item = pool.get(&clip.media_id()?)?;
                Some(klipp_compose::subject_matte::last_frame_secs(item.duration.to_secs_f64(), item.fps.as_f64()))
            });
            let short: Vec<String> = gaps
                .iter()
                .filter_map(|g| {
                    let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == g.clip)?;
                    let name = clip.media_id().and_then(|m| pool.get(&m)).map_or("a clip", |m| m.filename.as_str());
                    Some(match g.transition {
                        // A piece transition cutting round it (#985 phase 3).
                        Some((t, _)) => format!("{name} (transition {}): {} of {} frames tracked", t.0, g.covered, g.needed),
                        None => format!("{name} (clip {}): {} of {} frames tracked", g.clip.0, g.covered, g.needed),
                    })
                })
                .collect();
            if !short.is_empty() {
                return Err(format!("subject mattes aren't finished, export refused: {}. Run klipp_track_subject on each clip or transition and wait for its job.", short.join("; ")));
            }
            // What the export does not say for itself, the brief says.
            let brief = seq.brief.clone();
            // Whether what this sequence uses has credits (#1582): the list
            // itself is written when the export completes, of what it
            // played (#1604).
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
                // The sound alone, no picture (#1813).
                Some("mp3") => klipp_core::ExportFormat::Mp3,
                Some("wav") => klipp_core::ExportFormat::Wav,
                Some("m4a") => klipp_core::ExportFormat::M4a,
                Some(other) => return Err(format!("unsupported container .{other}; use mp4, mov, mkv or webm, or mp3, wav or m4a for the sound alone")),
            };
            let mut settings = klipp_core::ExportSettings { output_path: out.clone(), format, ..Default::default() };
            // The default H.264 and AAC do not go in WebM.
            settings.fit_codecs_to_format();
            // A preset first, so every field given beside it wins (#1583).
            let preset_name = match preset.as_deref() {
                Some(name) => Some(apply_export_preset(&mut settings, name)?),
                None => None,
            };
            if let Some(speed) = encoder_speed.as_deref() {
                settings.preset = encoder_speed_named(speed)?;
            }
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
                set_crf(&mut settings, c);
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
            // Asked of an encoder with no speed to set, it would be taken
            // and do nothing (#1899).
            if encoder_speed.is_some() {
                if let Some(why) = speed_ignored(&settings) {
                    return Err(format!("encoder_speed does nothing here: {why}; leave it out"));
                }
            }
            // Loudness: as asked, else the preset's own, else the brief's. A
            // preset with no target of its own (a master) used to drop the
            // brief's, unsaid (#2481).
            let loudness_from = match target_lufs {
                Some(l) => {
                    settings.target_lufs = Some(l);
                    "the call"
                }
                None if settings.target_lufs.is_some() => "the preset",
                None if brief_lufs.is_some() => {
                    settings.target_lufs = brief_lufs;
                    "the brief"
                }
                None => "nothing",
            };
            // Said when the delivery is not at the brief's loudness.
            let loudness_warning = brief_lufs.and_then(|b| match settings.target_lufs {
                Some(t) if (t - b).abs() > 0.5 => Some(format!("normalised to {t:.1} LUFS from {loudness_from}, not the brief's {b:.1} LUFS")),
                _ => None,
            });
            if let Some(ceiling) = true_peak_dbtp {
                if !ceiling.is_finite() || !(-10.0..=0.0).contains(&ceiling) {
                    return Err(format!("a true-peak ceiling of {ceiling} dBTP: give one from -10 to 0, -2 for ATSC A/85"));
                }
                settings.true_peak_dbtp = Some(ceiling);
            }
            if let Some(burn) = burn_captions {
                settings.burn_in_subtitles = burn;
            }
            if let Some(cc) = captions_608 {
                if cc && !matches!(settings.video_codec, V::H264 | V::H265) {
                    return Err(format!("{} carries no CEA-608 captions; captions_608 needs h264 or hevc", settings.video_codec.label()));
                }
                settings.captions_608 = cc;
            }
            // The output transfer, under the dialog's rules (#2477): HDR in
            // ProRes, or at 10 bits in h264 or hevc; an output transform
            // that does not serve the delivery is dropped, as there.
            let hdr_warning = match hdr.as_deref() {
                None => None,
                Some(name) => {
                    let mode = hdr_mode_named(name)?;
                    if mode != klipp_core::HdrMode::Sdr {
                        let prores = matches!(settings.video_codec, V::ProRes422 | V::ProRes422HQ | V::ProRes4444);
                        if !prores {
                            if !matches!(settings.video_codec, V::H264 | V::H265) {
                                return Err(format!("{} writes no HDR; hdr {name} needs hevc, h264 or ProRes", settings.video_codec.label()));
                            }
                            if bit_depth == Some(8) {
                                return Err(format!("hdr {name} is written at 10 bits; leave bit_depth out or give 10"));
                            }
                            settings.bit_depth = 10;
                        }
                    }
                    settings.hdr_mode = mode;
                    if !settings.odt.fits(mode) {
                        settings.odt = klipp_core::OdtChoice::MatchViewer;
                    }
                    settings.video_codec.hdr_advice(mode).map(|a| a.headline.to_string())
                }
            };
            if let Some(peak) = peak_nits {
                if settings.delivery() != klipp_core::HdrMode::Pq || settings.odt.is_pq() {
                    return Err("peak_nits is a PQ delivery's mastering peak; give hdr pq with it (HLG is mastered at 1000 nits)".into());
                }
                if !peak.is_finite() || !klipp_core::MASTERING_PEAK_RANGE.contains(&peak) {
                    return Err(format!("a mastering peak of {peak} nits: give 1000, 2000 or 4000"));
                }
                settings.mastering_peak_nits = peak;
            }
            if let Some(on) = hdr10plus {
                if on && !(matches!(settings.video_codec, V::H265) && settings.delivery() == klipp_core::HdrMode::Pq) {
                    return Err("hdr10plus needs a PQ delivery in hevc".into());
                }
                settings.hdr10plus = on;
            }
            // The range it covers: the In and Out points when both are set,
            // as the Export page's range does (#2667).
            let range = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                match (seq.in_point, seq.out_point) {
                    (Some(a), Some(b)) => json!({
                        "in": (a.to_secs_f64() * 1000.0).round() / 1000.0,
                        "out": (b.to_secs_f64() * 1000.0).round() / 1000.0,
                        "from": "the In and Out points; clear them to export the whole timeline",
                    }),
                    _ => json!("the whole timeline"),
                }
            };
            // Said, because it cannot be seen from the reply otherwise and
            // decides whether a sidecar file is needed.
            let said = json!({
                "range": range,
                "captions_burnt_in": settings.burn_in_subtitles,
                // Titles are part of the picture; a title track left out
                // of a delivery is hidden (#2426).
                "titles_burnt_in": true,
                "captions_608": settings.captions_608,
                "hdr10plus": settings.hdr10plus,
                "hdr": match settings.delivery() { klipp_core::HdrMode::Sdr => "sdr", klipp_core::HdrMode::Hlg => "hlg", klipp_core::HdrMode::Pq => "pq" },
                "peak_nits": settings.renders_hdr().then(|| settings.hdr_peak_nits()),
                "hdr_warning": hdr_warning,
                "target_lufs": settings.target_lufs,
                "loudness_from": loudness_from,
                "loudness_warning": loudness_warning,
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
                // The speed used: as asked, else the preset's own, else
                // medium; none for an encoder that has none.
                "encoder_speed": speed_ignored(&settings).is_none().then(|| settings.preset.ffmpeg_value()),
                "rate": match (settings.rate_control, settings.video_bitrate_kbps) {
                    (klipp_core::RateControl::Crf, _) | (_, None) => format!("crf {}", settings.crf),
                    (_, Some(kbps)) => format!("{kbps} kbps"),
                },
                "bit_depth": if settings.bit_depth == 10 { 10 } else { 8 },
            });
            let burnt = said["captions_burnt_in"].as_bool().unwrap_or(true);
            let known_jobs: std::collections::HashSet<String> = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner())
                .iter().map(|job| job.job_id.clone()).collect();
            let mut v = send(AppCommand::ExportSequence { settings })?;
            v["export"] = said;
            if !credits.is_empty() {
                v["credits"] = json!({
                    "path": klipp_audio_search::credits_path(&out),
                    "when": "written when the export completes: the credits of what it plays, in its range, on heard tracks",
                });
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

        ControlCommand::ExportSubtitles { path, format } => {
            let p = std::path::PathBuf::from(&path);
            if !p.is_absolute() {
                return Err(format!("subtitle path must be absolute (got {path:?})"));
            }
            let ext = subtitle_format(&p).ok_or_else(|| format!("{path:?} names no subtitle format; end it .srt, .vtt, .ass, .ssa, .ttml, .scc or .stl"))?;
            if let Some(f) = format.as_deref() {
                let want = f.trim().to_ascii_lowercase();
                let want = match want.as_str() {
                    "webvtt" => "vtt".to_string(),
                    "imsc" => "ttml".to_string(),
                    _ => want,
                };
                if !SUBTITLE_FORMATS.contains(&want.as_str()) {
                    return Err(format!("{f:?} is not a subtitle format; give srt, vtt, ass, ssa, ttml, scc or stl"));
                }
                // TTML's extensions all write IMSC.
                if ttml_family(&want) != ttml_family(ext) {
                    return Err(format!("the path ends .{ext} but the format asked for is {want}; end the path .{want}"));
                }
            }
            // A file a subtitle track when there are several (#2223).
            let files = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                klipp_document::subtitle_export::files(&seq, &p)
            };
            let mut v = send(AppCommand::ExportSubtitles { path: p })?;
            v["files"] = json!(files.iter().map(|f| json!({ "path": f.path.display().to_string(), "tracks": f.tracks, "language": f.language })).collect::<Vec<_>>());
            Ok(v)
        }

        ControlCommand::TranslateCaptions { track, to, from, download_models, glossary } => {
            use klipp_document::caption_translation as ct;
            use klipp_ml_candle::translation as tr;
            if in_batch {
                return Err("translate_captions runs a translation model; send it on its own".into());
            }
            let to = to.trim().to_ascii_lowercase();
            let pairs = || tr::PAIRS.iter().map(|p| format!("{}→{}", p.source, p.target)).collect::<Vec<_>>().join(", ");
            if !tr::PAIRS.iter().any(|p| p.target == to) {
                return Err(format!("no translation into {to}; the pairs are {}", pairs()));
            }
            let (track_id, sentences, tagged) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let ti = match track {
                    Some(t) => t,
                    None => seq
                        .tracks
                        .iter()
                        .enumerate()
                        .position(|(i, t)| t.kind == klipp_timeline::TrackKind::Subtitle && !ct::captions(&seq, i).is_empty())
                        .ok_or("the sequence has no subtitle track with captions; make captions first (the captions op)")?,
                };
                let t = seq.tracks.get(ti).ok_or_else(|| format!("no track {ti}"))?;
                if !matches!(t.kind, klipp_timeline::TrackKind::Subtitle | klipp_timeline::TrackKind::Text) {
                    return Err(format!("track {ti} is not a subtitle or text track"));
                }
                // The track's own language, when it is tagged with one
                // (set_track language, or a translation's): the captions
                // are in it (#2489). A region (pt-br) is the language's.
                let tagged = t.language.as_deref().map(|l| l.split('-').next().unwrap_or(l).to_string());
                (t.id, ct::sentences(&ct::captions(&seq, ti)), tagged)
            };
            let from = match from.map(|f| f.trim().to_ascii_lowercase()).or(tagged) {
                Some(f) => f,
                None if to == "en" => return Err("name the captions' language with from (de, fr, es, sv, it or nl), or tag the track with it (set_track language), to translate into English".into()),
                None => "en".into(),
            };
            let pair = tr::pair(&from, &to).ok_or_else(|| format!("no translation from {from} to {to}; the pairs are {}", pairs()))?;
            if sentences.is_empty() {
                return Err("that track has no captions to translate".into());
            }
            if !download_models {
                tr::ensure_pair(&pair, false).map_err(|e| format!("{e}; send download_models true to fetch it (about 300 MB, CC BY 4.0, Helsinki-NLP OPUS-MT)"))?;
            }
            if let Some(running) = tr::jobs::running() {
                return Err(format!("translation {} into {} is running already; follow it with job_status, or cancel_job it", running.id, running.language));
            }
            // The app runs it, as its menu does: a job with progress and
            // cancel, the track added when it is done.
            let job_id = uuid::Uuid::new_v4().to_string();
            let glossary: Vec<(String, String)> = glossary.into_iter().filter(|(from, _)| !from.trim().is_empty()).collect();
            let mut v = send(AppCommand::TranslateCaptions { track_id, source: from.clone(), target: to.clone(), job_id: job_id.clone(), glossary })?;
            v["job_id"] = json!(job_id);
            v["sentences"] = json!(sentences.len());
            v["text"] = json!(format!("Translating {} sentences into {} as job {job_id}.", sentences.len(), tr::language_name(&to)));
            v["next"] = json!("Follow it with follow_job or job_status; when it completes its result names the new track and the captions to check. cancel_job stops it.");
            Ok(v)
        }

        ControlCommand::ImportTimeline { path, new_project } => {
            if in_batch { return Err("import_timeline must be sent on its own".into()); }
            let Some(path) = path else {
                return Ok(ctx.shared.import_outcome.lock().map_err(|_| "import report lock poisoned")?.clone().unwrap_or(json!({"report":null})));
            };
            let path = std::path::PathBuf::from(path);
            if !path.is_absolute() || !path.is_file() { return Err("import path must name an existing absolute file".into()); }
            // The format is told from the file's content where it is read
            // (#2083); one it is not is refused there, by name.
            let request_id = uuid::Uuid::new_v4();
            let command = if new_project { AppCommand::OpenFromAnotherEditor { path, request_id } } else { AppCommand::ImportTimeline { path, request_id } };
            let mut result = send(command)?;
            result["request_id"] = json!(request_id);
            let outcome = ctx.shared.import_outcome.lock().map_err(|_| "import report lock poisoned")?.clone();
            if let Some(outcome) = outcome.filter(|o| o["request_id"] == json!(request_id)) {
                if let Some(error) = outcome["error"].as_str() { return Err(error.to_string()); }
                result["report"] = outcome["report"].clone();
            } else {
                result["next"] = json!("Read import_timeline without path and match request_id for the report/error; do not repeat the import.");
            }
            Ok(result)
        }

        ControlCommand::ExportTimeline { path, format, overwrite } => {
            if in_batch {
                return read_in_batch("export_timeline");
            }
            let out = std::path::PathBuf::from(&path);
            if !out.is_absolute() || !out.parent().is_some_and(|p| p.is_dir()) {
                return Err(format!("export path must be absolute, in a folder that exists (got {path:?})"));
            }
            let ext = out.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            let Some(kind) = interchange_format(&ext) else {
                return Err(format!("{path:?} names no interchange format: end it in .fcpxml, .edl, .otio, .aaf or .kdenlive"));
            };
            if let Some(f) = format.as_deref().map(str::to_ascii_lowercase) {
                if interchange_format(&f) != Some(kind) {
                    return Err(format!("format {f:?} is not the path's .{ext}"));
                }
            }
            if out.exists() && !overwrite {
                return Err(format!("{path:?} exists: send overwrite true to replace it"));
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.clone();
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?.clone();
            let dropped: Vec<String> = match kind {
                "fcpxml" => klipp_project::fcpxml::export_fcpxml(&seq, &pool, &out)?,
                "edl" => klipp_project::fcpxml::export_edl(&seq, &pool, &out)?,
                "kdenlive" => klipp_project::mlt_export::export_kdenlive(&seq, &pool, &out)?,
                "otio" => klipp_project::otio_export::export_otio(&seq, &pool, &out).map_err(|e| e.to_string())?,
                _ => {
                    // AAF: OpenTimelineIO's own file first, then its
                    // converter, as the menu's export does.
                    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    let otio = std::env::temp_dir().join(format!("klipp-aaf-{}-{stamp}.otio", std::process::id()));
                    let dropped = klipp_project::otio_export::export_otio(&seq, &pool, &otio).map_err(|e| e.to_string());
                    let converted = dropped.is_ok().then(|| klipp_project::otio_export::convert(&otio, &out));
                    let _ = std::fs::remove_file(&otio);
                    let install = "install OpenTimelineIO and its AAF adapter (pip install opentimelineio otio-aaf-adapter), or export .otio and convert it";
                    match converted {
                        Some(Err(klipp_project::otio_export::ConvertError::NoConverter | klipp_project::otio_export::ConvertError::NoAdapter(_))) => return Err(format!("AAF export needs OpenTimelineIO: {install}")),
                        Some(Err(e)) => return Err(format!("AAF export failed: {e}")),
                        _ => dropped?,
                    }
                }
            };
            let clips = seq.tracks.iter().flat_map(|t| &t.clips).filter(|c| !c.kind.is_gap()).count();
            let text = match dropped.len() {
                0 => format!("\"{}\" written as {kind} to {path}: {clips} clips", seq.name),
                n => format!("\"{}\" written as {kind} to {path}: {clips} clips; {n} thing(s) the format cannot carry were left out (dropped lists them)", seq.name),
            };
            Ok(json!({ "path": path, "format": kind, "sequence": seq.name, "clips": clips, "dropped": dropped, "text": text }))
        }

        ControlCommand::ImportSubtitles { path } => {
            let p = std::path::PathBuf::from(&path);
            if !p.is_absolute() {
                return Err(format!("subtitle path must be absolute (got {path:?})"));
            }
            if subtitle_format(&p).is_none() {
                return Err(format!("{path:?} is not a subtitle file this reads; give an .srt, .vtt, .ass, .ssa, .ttml, .dfxp, .xml, .itt, .scc or .stl"));
            }
            if !p.is_file() {
                return Err(format!("no file at {path:?}"));
            }
            let language = std::fs::read(&p).ok().and_then(|bytes| klipp_captions::file_language(&p, &bytes));
            let mut v = send(AppCommand::ImportSubtitles { path: p })?;
            v["language"] = json!(language);
            v["next"] = json!("The cues land on a new subtitle track named after the file, in the language the file names (language, null when it names none); timeline shows it.");
            Ok(v)
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

        ControlCommand::RecentProjects => {
            if in_batch { return read_in_batch("recent_projects"); }
            Ok(json!({"projects": crate::project_access::recent(&project_status(ctx)?)}))
        }

        ControlCommand::CloseProject { save, discard_changes } => {
            if in_batch { return Err("close_project is not part of a batch; send it on its own".into()); }
            let before = project_status(ctx)?;
            before.replacement_save_path(before.identity, save, discard_changes)?;
            let result = crate::project_access::switch(ctx, AppCommand::CloseProjectNow { save, discard_changes, expected_identity: before.identity })?;
            if result["applied"] != true { return Ok(result); }
            Ok(json!({"closed":true,"operation_id":result["operation_id"],"revision":result["revision"]}))
        }

        ControlCommand::OpenProject { path, name, save, discard_changes, recovery } => {
            if in_batch { return Err("open_project is not part of a batch; send it on its own".into()); }
            let recovery = match recovery.as_deref() {
                None => None,
                Some("restore") => Some(true),
                Some("keep") => Some(false),
                Some(other) => return Err(format!("recovery \"{other}\" is not one: restore or keep")),
            };
            let before = project_status(ctx)?;
            let p = crate::project_access::resolve(&before, path.as_deref(), name.as_deref())?;
            before.replacement_save_path(before.identity, save, discard_changes)?;
            // Unsaved work from a crash (#2789): said before anything opens,
            // for the caller to choose.
            let found = klipp_project::recovery::newer_autosave(&p);
            if let (Some(found), None) = (&found, recovery) {
                let when = found.autosaved_at.as_deref().unwrap_or("an unknown time");
                return Ok(json!({
                    "opened": false,
                    "path": p.to_string_lossy(),
                    "newer_autosave": { "autosaved_at": found.autosaved_at, "saved_at": found.saved_at },
                    "text": format!("Not opened: this project has an auto-save from {when}, newer than its last save ({}), holding unsaved work, usually from a crash.", found.saved_at.as_deref().unwrap_or("never saved")),
                    "next": "Open it again with recovery \"restore\" to open the auto-save's work (unsaved until saved), or \"keep\" to open the saved version and set the auto-save aside.",
                }));
            }
            let result = crate::project_access::switch(ctx, AppCommand::OpenProjectNow { path: p.clone(), save, discard_changes, expected_identity: before.identity, recovery })?;
            if result["applied"] != true { return Ok(result); }
            let offline = super::media::offline_media(ctx);
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
            Ok(json!({
                "offline_media": offline,
                "next": offline.as_ref().map(|o| o["text"].clone()),
                "opened": true,
                "recovered": found.is_some() && recovery == Some(true),
                "operation_id": result["operation_id"],
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
            let (data, width, height, clip) = grab_picture_rgba(ctx, secs, timeout_ms)?;
            let mut v = frame_stats(&data, width, height, secs);
            let faces = skin_regions(ctx, secs, &data, width, height);
            let mut m = klipp_color::measure::measure_frame(&data, width, height, &faces);
            let look = look_for(ctx, secs);
            if needs_source(&klipp_color::measure::judge(&m, &look), &look) {
                m.source = picture_clip_at(ctx, secs).and_then(|c| source_reading(ctx, c, secs));
            }
            let findings = klipp_color::measure::judge(&m, &look);
            let mut text = v["text"].as_str().unwrap_or("").to_string();
            // A face read in the frame backs the skin line; without one
            // it stays a colour reading (#1897).
            // Read inside the faces, so warm stone beside them does not
            // move it (#1897 review).
            let regions: Vec<[f32; 4]> = m.faces.iter().filter(|f| f.readable()).map(|f| f.region).collect();
            let read_faces = regions.len();
            if let Some((pct, _)) = (read_faces > 0).then(|| skin_line_share(&data, width, height, &regions)).flatten() {
                v["skin_on_line_pct"] = json!(pct);
                let note = skin_colour_note(pct, read_faces);
                if let Some(old) = v["skin"].as_str() {
                    text = text.replace(&format!("skin: {old}"), &format!("skin: {note}"));
                }
                v["skin"] = json!(note);
                v["skin_from"] = json!("faces");
            }
            text.push_str(&format!("\nLab (D65): L* p1 {:.0} p50 {:.0} p99 {:.0}, contrast {:.0}, chroma mean {:.0}; neutrals a* {:.1} b* {:.1} ({}, {:.0}% of the frame)", m.l_p1, m.l_p50, m.l_p99, m.contrast_range, m.chroma_mean, m.cast_a, m.cast_b, if m.cast() < 3.0 { "neutral".to_string() } else { format!("cast toward {}", klipp_color::measure::cast_name(m.cast_a, m.cast_b)) }, m.neutral_pct));
            for (i, f) in m.faces.iter().enumerate() {
                if f.readable() {
                    text.push_str(&format!("\nface {}: skin hue {:.0}° (band {:.0}–{:.0}), chroma {:.0}, L* {:.0}, {} of {}×{} patches, hue spread {:.0}°{}", i + 1, f.hue, klipp_color::measure::SKIN_HUE_LOW, klipp_color::measure::SKIN_HUE_HIGH, f.chroma, f.median.l, f.patches, f.grid, f.grid, f.hue_iqr, if f.mixed_light { " (two lights)" } else { "" }));
                } else if f.patches > 0 {
                    text.push_str(&format!("\nface {}: too little skin to read ({} of {}×{} patches at {:.0}%,{:.0}% of the frame)", i + 1, f.patches, f.grid, f.grid, (f.region[0] + f.region[2] / 2.0) * 100.0, (f.region[1] + f.region[3] / 2.0) * 100.0));
                }
            }
            text.push_str(&format!("\nAgainst the look \"{}\": {}", look.name, findings.iter().filter(|f| f.verdict != klipp_color::measure::Verdict::Pass).map(|f| format!("{:?} {}", f.verdict, f.what)).collect::<Vec<_>>().join("; ").replace("Fail", "FAIL").replace("Warn", "warn")));
            if let Some(c) = clip {
                text.push_str("\nRead from the picture clip alone: titles and clips above it, and its fill, shadow or glow, are left out.");
                v["clip_id"] = json!(c.0.to_string());
            }
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
                let names = times.iter().map(|t| shown_at(&seq, &pool, *t)).collect();
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
                    // A file under the target that cannot be decoded is
                    // why: said by name, not as where the viewer stopped.
                    if !at_target {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                        if let Some(why) = undecodable_picture_at(&seq, &pool, target) {
                            return Err(format!("no frame at {secs:.3} s: {why}"));
                        }
                    }
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
            // Wider than the viewer's frame, which is decoded at the
            // playback size, and the timeline has it: drawn from the files
            // at that size, as the export draws it (#2491).
            let viewer_width = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_frame().1;
            let (seq_w, seq_h) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                (seq.resolution.width.max(2), seq.resolution.height.max(2))
            };
            if let Some((w, h)) = full_size_for(max_width, viewer_width, (seq_w, seq_h)) {
                restore_transport(ctx, held);
                let still = full_size_frame(ctx, secs, w, h)?;
                if let Some(p) = path.as_deref().filter(|p| !std::path::Path::new(p).is_absolute()) {
                    return Err(format!("screenshot path must be absolute (got {p:?})"));
                }
                let out = path.map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("klipp-frame-{}.png", uuid::Uuid::new_v4())));
                write_png(&out, &still.rgba, still.width, still.height)?;
                return Ok(json!({
                    "path": out.to_string_lossy(), "width": still.width, "height": still.height,
                    "secs": secs, "frame": (secs * fps.as_f64()).round() as i64,
                    "drawn": "from the files at this size, as the export draws it; the viewer shows the playback size",
                }));
            }
            let shot = handle(
                ctx,
                ControlCommand::Screenshot { path, max_width: Some(max_width.unwrap_or(1280)), min_generation: Some(generation), timeout_ms },
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
                    "phases": phases(&p.phases),
                })),
                "average_of_last_60": {
                    "decode_ms": avg.decode_ms,
                    "grade_ms": avg.grade_ms,
                    "total_ms": avg.total_ms,
                    // Where a viewer frame's time went, around its render
                    // (#1850); 0 for a phase a frame didn't take.
                    "phases": phases(&avg.phases),
                },
                "picture_fallbacks": klipp_effects::residency::snapshot(),
                "generators": {
                    "gpu_draws": klipp_compose::GPU_GENERATOR_DRAWS.load(std::sync::atomic::Ordering::Relaxed),
                    "cpu_draws": klipp_compose::CPU_GENERATOR_DRAWS.load(std::sync::atomic::Ordering::Relaxed),
                    "cpu_total_us": klipp_compose::CPU_GENERATOR_US.load(std::sync::atomic::Ordering::Relaxed),
                    "scope": "cumulative process totals; CPU cache hits excluded",
                },
                "effects_readbacks": {
                    "totals": klipp_effects::gpu::readback_stats(),
                    "scope": "cumulative process totals: blocking effects/title-mask downloads; excludes engine conversions, async export staging and other GPU subsystems; elapsed includes queue wait and CPU copy-out, overlaps render time",
                },
                "readbacks": {
                    "scope": "cumulative process totals: engine GPU-frame conversions only; not all GPU transfers",
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
                    // matte made apart and blended on the CPU (9c-2 B2): a
                    // key's, which reads the grade's picture.
                    "blended_on_cpu": klipp_compose::EFFECTS_BLENDED_ON_CPU.load(std::sync::atomic::Ordering::Relaxed),
                    // Those whose matte is a window, drawn and blended in
                    // the effect's own plan, on the GPU (#1265).
                    "blended_apart": klipp_compose::EFFECTS_BLENDED_APART.load(std::sync::atomic::Ordering::Relaxed),
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
/// A viewer frame's phases (#1850), as the `perf` reply gives them.
/// The interchange format an extension or `format` names.
fn interchange_format(name: &str) -> Option<&'static str> {
    match name {
        "fcpxml" => Some("fcpxml"),
        "edl" => Some("edl"),
        "otio" => Some("otio"),
        "aaf" => Some("aaf"),
        "kdenlive" => Some("kdenlive"),
        _ => None,
    }
}

/// What a sheet's tile at `secs` shows: the pictures, and titles over
/// them. Of a multicam group, only the angle its cut list has live there:
/// the others are under the playhead too, and every angle used to be
/// named, top track first, whichever was seen (#2419).
fn shown_at(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, secs: f64) -> String {
    let at = klipp_core::Timecode::from_secs_f64(secs, seq.fps.rate());
    let live = |c: &klipp_timeline::Clip| match c.multicam_group.and_then(|g| seq.multicam_at(Some(g), at)) {
        Some(m) => c.multicam_angle == m.active,
        None => true,
    };
    seq.tracks
        .iter()
        .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text) && t.visible)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled && !c.consumed_as_matte && c.timeline_in.to_secs_f64() <= secs && c.timeline_out.to_secs_f64() > secs && live(c))
        .map(|c| match &c.kind {
            klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "media".into()),
            klipp_timeline::ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("").chars().take(24).collect::<String>()),
            klipp_timeline::ClipKind::Generator(_) => "generator".into(),
            _ => "clip".into(),
        })
        .collect::<Vec<_>>()
        .join(" over ")
}

fn phases(p: &klipp_engine::perf::FramePhases) -> serde_json::Value {
    json!({
        "sync_ms": p.sync_ms,
        "snapshot_ms": p.snapshot_ms,
        "frame_snapshot_ms": p.frame_snapshot_ms,
        "render_ms": p.render_ms,
        "present_ms": p.present_ms,
    })
}

/// The encoder speeds an export takes, by the name an agent gives one.
pub const ENCODER_SPEEDS: [&str; 4] = ["fast", "medium", "slow", "veryslow"];

/// An encoder speed by name (#1899): how long the encoder spends on each
/// frame, apart from the delivery preset.
fn encoder_speed_named(name: &str) -> Result<klipp_core::EncoderPreset, String> {
    use klipp_core::EncoderPreset as E;
    [E::Fast, E::Medium, E::Slow, E::VerySlow]
        .into_iter()
        .find(|e| e.ffmpeg_value() == name.trim().to_ascii_lowercase().replace([' ', '_', '-'], ""))
        .ok_or_else(|| format!("no encoder speed {name:?}; give {}", ENCODER_SPEEDS.join(", ")))
}

/// Why the encoder `settings` choose has no speed to set, if it has none:
/// ProRes and DNxHD encode at their own fixed rates, a GIF and the sound
/// alone have no video encoder to pace (#1899).
fn speed_ignored(settings: &klipp_core::ExportSettings) -> Option<String> {
    use klipp_core::ExportVideoCodec as V;
    if settings.format.is_audio_only() {
        return Some("the sound alone has no video encoder".into());
    }
    if settings.format == klipp_core::ExportFormat::Gif {
        return Some("a GIF has no video encoder speed".into());
    }
    matches!(settings.video_codec, V::ProRes422 | V::ProRes422HQ | V::ProRes4444 | V::DNxHD115 | V::DNxHD145)
        .then(|| format!("{} encodes at its own fixed rate, with no speed setting", settings.video_codec.label()))
}

/// The built-in delivery presets an export takes by name, each with the
/// extension its path must end in (#1899).
pub fn export_presets() -> Vec<(String, &'static str)> {
    klipp_core::builtin_presets().into_iter().map(|p| (p.name, p.settings.format.extension())).collect()
}

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
    settings.bit_depth = s.bit_depth;
    Ok(p.name.clone())
}

/// Encode at quality `crf`: a crf given beside a preset that encodes to a
/// bitrate turns its rate control to crf, or the crf would go unused.
fn set_crf(settings: &mut klipp_core::ExportSettings, crf: u32) {
    settings.crf = crf;
    settings.rate_control = klipp_core::RateControl::Crf;
}

/// The size `frame_at` draws its frame at from the files (#2491): the
/// width asked for, up to the timeline's, when it is wider than the
/// viewer's frame; `None` when the viewer's frame does.
fn full_size_for(max_width: Option<u32>, viewer_width: u32, (seq_w, seq_h): (u32, u32)) -> Option<(u32, u32)> {
    let w = max_width?.min(seq_w) & !1;
    (w > viewer_width && w >= 2).then(|| (w, ((w as u64 * seq_h as u64 / seq_w.max(1) as u64) as u32).max(2) & !1))
}

/// The timeline's frame at `secs` drawn from the files at `width` x
/// `height`, with the project's colour set-up and the viewer's LUT
/// (#2491).
fn full_size_frame(ctx: &ControlContext, secs: f64, width: u32, height: u32) -> Result<klipp_render::still::Still, String> {
    let seq: klipp_timeline::Sequence = (*ctx.sequence.read().map_err(|_| "sequence lock poisoned")?).clone();
    let mut all: Vec<klipp_timeline::Sequence> = ctx.others.read().map_err(|_| "sequences lock poisoned")?.iter().map(|s| (*s.sequence).clone()).collect();
    if !all.iter().any(|s| s.id == seq.id) {
        all.push(seq.clone());
    }
    let lut = ctx.shared.project.lock().ok().and_then(|p| p.viewer_lut.clone());
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let mut snapshot = klipp_render::SequenceSnapshot::from_sequence_with_all(
        &seq,
        &all,
        &pool,
        &klipp_core::ExportSettings::default(),
        lut.as_ref().map(|(p, _)| p.clone()),
        false,
        klipp_engine::video_decoder::working_colorspace(),
    )
    .map_err(|e| e.to_string())?;
    drop(pool);
    snapshot.grade_encoding = klipp_core::grade_encoding();
    if let Some((_, opacity)) = lut {
        snapshot.video.global_lut_opacity = opacity;
    }
    klipp_render::still::timeline_still(&snapshot, secs, width, height).map_err(|e| format!("no frame at {secs:.3} s: {e}"))
}

/// An output transfer by the name an agent gives it (#2477).
fn hdr_mode_named(name: &str) -> Result<klipp_core::HdrMode, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sdr" | "rec709" | "bt709" => Ok(klipp_core::HdrMode::Sdr),
        "hlg" => Ok(klipp_core::HdrMode::Hlg),
        "pq" | "hdr10" => Ok(klipp_core::HdrMode::Pq),
        _ => Err(format!("{name:?} is not an output transfer; give sdr, hlg or pq")),
    }
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

/// The subtitle formats the sidecar reader and writers take, by extension:
/// TTML's (#1516) read as the family and written as IMSC 1.1 Text, and
/// SCC's CEA-608 captions and EBU-STL.
const SUBTITLE_FORMATS: &[&str] = &["srt", "vtt", "ass", "ssa", "ttml", "dfxp", "xml", "itt", "scc", "stl"];

/// A format as the one it reads and writes as: TTML's extensions as `ttml`.
fn ttml_family(f: &str) -> &str {
    if matches!(f, "dfxp" | "xml" | "itt") { "ttml" } else { f }
}

/// The subtitle format a path's extension names, lowercase.
fn subtitle_format(p: &std::path::Path) -> Option<&'static str> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    SUBTITLE_FORMATS.iter().copied().find(|f| *f == ext)
}

#[cfg(test)]
mod frame_size_tests {
    use super::full_size_for;

    /// #2491: asked wider than the viewer's frame, frame_at draws at the
    /// width asked, up to the timeline's, at its shape; else the viewer's.
    #[test]
    fn a_frame_is_drawn_at_the_width_asked_up_to_the_timelines() {
        let uhd = (3840, 2160);
        assert_eq!(full_size_for(Some(3840), 852, uhd), Some((3840, 2160)));
        assert_eq!(full_size_for(Some(8000), 852, uhd), Some((3840, 2160)), "no wider than the timeline");
        assert_eq!(full_size_for(Some(1921), 852, uhd), Some((1920, 1080)), "even sides");
        assert_eq!(full_size_for(Some(640), 852, uhd), None, "the viewer's frame is wide enough");
        assert_eq!(full_size_for(None, 852, uhd), None, "no width asked: the viewer's");
        assert_eq!(full_size_for(Some(1920), 1920, (1920, 1080)), None);
    }
}

#[cfg(test)]
mod credits_tests {
    use super::sequence_credits;

    /// #1607 review: crf beside a bitrate preset encodes at that crf.
    #[test]
    fn a_crf_turns_the_rate_control_to_crf() {
        let mut s = klipp_core::ExportSettings { rate_control: klipp_core::RateControl::Vbr, video_bitrate_kbps: Some(8000), ..Default::default() };
        super::set_crf(&mut s, 20);
        assert_eq!((s.crf, s.rate_control), (20, klipp_core::RateControl::Crf));
    }

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
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod sheet_tests {
    use klipp_core::{GroupId, MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    /// #2419: a tile names the angle its multicam cut has live, not every
    /// angle under the playhead, and a clip in no group as before.
    #[test]
    fn a_tile_names_the_live_angle() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let mut pool = klipp_media::MediaPool::new();
        let mut file = |name: &str| {
            let mut item = klipp_media::MediaItem::synthetic_for_test(format!("/tmp/{name}").into(), None);
            item.filename = name.into();
            pool.add(item)
        };
        let (wide, close, other): (MediaId, MediaId, MediaId) = (file("wide.mp4"), file("close.mkv"), file("other.mp4"));
        let g = GroupId::new();
        let angle = |m: MediaId, n: u8| {
            let mut c = Clip::media(m, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
            (c.multicam_group, c.multicam_angle) = (Some(g), n);
            c
        };
        let mut primary = angle(wide, 0);
        primary.multicam_cuts = vec![(4.0, 1)];
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        seq.tracks = [vec![angle(close, 1)], vec![primary], vec![Clip::media(other, tc(10.0), tc(12.0), tc(0.0), tc(2.0))]]
            .into_iter()
            .map(|clips| Track { clips, ..Track::new("V", TrackKind::Video) })
            .collect();
        assert_eq!(super::shown_at(&seq, &pool, 2.0), "wide.mp4");
        assert_eq!(super::shown_at(&seq, &pool, 6.0), "close.mkv");
        assert_eq!(super::shown_at(&seq, &pool, 11.0), "other.mp4");
    }
}

#[cfg(test)]
mod export_choice_tests {
    use super::*;

    /// #1899: the encoder's speed is its own field, by the names the
    /// schema lists; a delivery preset's name isn't one, and an unknown
    /// speed is refused with the four there are.
    #[test]
    fn an_encoder_speed_is_its_own_field() {
        use klipp_core::EncoderPreset as E;
        let got: Vec<E> = ENCODER_SPEEDS.iter().map(|n| encoder_speed_named(n).unwrap()).collect();
        assert_eq!(got, [E::Fast, E::Medium, E::Slow, E::VerySlow]);
        assert_eq!(encoder_speed_named("Very Slow").unwrap(), E::VerySlow);
        let err = encoder_speed_named("turbo").unwrap_err();
        assert!(ENCODER_SPEEDS.iter().all(|s| err.contains(s)), "{err}");
        // The speed set after a preset wins over the preset's own.
        let mut settings = klipp_core::ExportSettings { format: klipp_core::ExportFormat::Mp4, ..Default::default() };
        let (name, _) = export_presets().into_iter().find(|(_, ext)| *ext == "mp4").expect("an mp4 preset");
        apply_export_preset(&mut settings, &name).unwrap();
        settings.preset = encoder_speed_named("slow").unwrap();
        assert_eq!(settings.preset, E::Slow);
        assert!(apply_export_preset(&mut settings, "slow").is_err(), "a speed is not a delivery preset");
    }

    /// #1899: a speed asked of an encoder that has none is refused, not
    /// taken and ignored: ProRes, DNxHD, a GIF, the sound alone.
    #[test]
    fn an_encoder_speed_is_refused_where_nothing_reads_it() {
        use klipp_core::{ExportFormat as F, ExportVideoCodec as V};
        let with = |format: F, codec: V| klipp_core::ExportSettings { format, video_codec: codec, ..Default::default() };
        assert!(speed_ignored(&with(F::Mov, V::ProRes422HQ)).unwrap().contains("fixed rate"));
        assert!(speed_ignored(&with(F::Mov, V::DNxHD145)).is_some());
        assert!(speed_ignored(&with(F::Gif, V::H264)).is_some());
        assert!(speed_ignored(&with(F::Mp3, V::H264)).is_some());
        assert_eq!(speed_ignored(&with(F::Mp4, V::H264)), None);
        assert_eq!(speed_ignored(&with(F::Mkv, V::Av1)), None);
    }

    /// #1899: every listed preset is one the export takes, at its listed
    /// container.
    #[test]
    fn every_listed_preset_is_taken() {
        let presets = export_presets();
        assert!(presets.len() > 3);
        for (name, ext) in presets {
            let format = klipp_core::builtin_presets().into_iter().find(|p| p.name == name).unwrap().settings.format;
            assert_eq!(format.extension(), ext);
            let mut settings = klipp_core::ExportSettings { format, ..Default::default() };
            assert_eq!(apply_export_preset(&mut settings, &name).unwrap(), name);
        }
    }
}
