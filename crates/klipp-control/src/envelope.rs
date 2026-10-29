//! Sending to the app and waiting for its answer: envelopes, acks, the undo
//! fold, the status text, and the small helpers they share (#826).

#[allow(unused_imports)]
use super::*;

/// The pool item for a file just imported, waited for: the import is
/// the app's to finish, and it answers before the item is in the pool.
pub(crate) fn imported_id(ctx: &ControlContext, path: &std::path::Path) -> Option<String> {
    imported_id_within(ctx, path, std::time::Duration::from_secs(10))
}

pub(crate) fn imported_id_within(ctx: &ControlContext, path: &std::path::Path, wait: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Ok(pool) = ctx.media_pool.read() {
            if let Some(item) = pool.find_by_path(path) {
                return Some(item.id.0.to_string());
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// A program on the PATH, if it is there.
pub(crate) fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// Seconds since the epoch, for a file name.
pub(crate) fn chrono_stamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}


pub(crate) fn temp_output(path: Option<String>, stem: &str, ext: &str) -> Result<std::path::PathBuf, String> {
    match path {
        Some(p) => {
            let p = std::path::PathBuf::from(p);
            if !p.is_absolute() {
                return Err(format!("path must be absolute (got {})", p.display()));
            }
            Ok(p)
        }
        None => Ok(std::env::temp_dir().join(format!(
            "{stem}-{}.{ext}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
        ))),
    }
}

/// A frame rate from a number: the NTSC rates as their exact fractions,
/// anything else to a thousandth.
pub(crate) fn fps_from_f64(f: f64) -> klipp_core::Fps {
    for (num, den) in [(24000u32, 1001u32), (30000, 1001), (60000, 1001), (48000, 1001)] {
        if (f - num as f64 / den as f64).abs() < 0.005 {
            return klipp_core::Fps::new(num, den);
        }
    }
    klipp_core::Fps::new((f * 1000.0).round().max(1.0) as u32, 1000)
}

/// Why a request could not be read, in terms an agent can act on (#729).
/// A whole request's parse error points at a byte offset in the line and
/// at serde's names ("untagged enum ClipRef"); a list of ops is read one
/// op at a time instead, so the reply names the step, its op and what
/// went wrong in it, with what a clip can be named by.
pub(crate) fn explain_bad_request(line: &str, e: &serde_json::Error) -> String {
    // Any other request: the field the error is in, by its path (#783).
    let fallback = match json_path::explain(line, e) {
        Some(why) => format!("bad request: {why}"),
        None => format!("bad request: {e}"),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { return fallback };
    let ops = value.get("ops").or_else(|| value.get("command").and_then(|c| c.get("ops"))).and_then(|o| o.as_array());
    let Some(ops) = ops else { return fallback };
    for (i, op) in ops.iter().enumerate() {
        if let Err(err) = serde_json::from_value::<klipp_document::ops::Op>(op.clone()) {
            let name = op.get("op").and_then(|n| n.as_str()).unwrap_or("?");
            let why = err.to_string();
            let hint = if why.contains("ClipRef") {
                " A clip is named by its id (a string from the timeline view) or by {\"track\": index, \"secs\": time}; a grade group or the whole timeline is graded with \"on\": {\"group\": name, \"stage\": \"pre\" or \"post\"} or \"on\": \"timeline\", not clip_id."
            } else if why.contains("unknown variant") {
                " klipp_guide lists the ops and what each takes."
            } else {
                ""
            };
            return format!("step {} ({name}) could not be read: {}.{hint}", i + 1, why.split(" at line ").next().unwrap_or(&why));
        }
    }
    fallback
}

/// What [`status_text`] says Klipp is.
pub(crate) struct StatusFacts<'a> {
    pub(crate) build: &'a klipp_core::build_info::BuildInfo,
    pub(crate) project: Option<&'a std::path::Path>,
    pub(crate) unsaved: bool,
    pub(crate) sequence: &'a klipp_timeline::Sequence,
    pub(crate) media: usize,
    pub(crate) playhead_secs: f64,
    pub(crate) playing: bool,
    /// Every sequence of the project and the open one's index.
    pub(crate) sequences: &'a [klipp_core::SequenceSummary],
    pub(crate) active_sequence: usize,
}

/// Every sequence of the project in one line, the open one marked, so a
/// reader who sees one timeline knows whether there are others. The open
/// one's length is `open_secs`, read live: the list can lag an edit.
pub(crate) fn sequences_text(list: &[klipp_core::SequenceSummary], active: usize, open_secs: f64) -> String {
    let each: Vec<String> = list.iter().enumerate().map(|(i, s)| {
        let open = i == active;
        let secs = if open { open_secs } else { s.duration_secs };
        format!("[{i}] {} {secs:.1} s, {} clip{}{}", quoted(&s.name), s.clips, if s.clips == 1 { "" } else { "s" }, if open { " (open)" } else { "" })
    }).collect();
    match list.len() {
        0 => "Sequences: none.".to_string(),
        1 => format!("Sequences: 1, the open one: {}.", each[0]),
        n => format!("Sequences: {n}; switch_sequence opens another: {}.", each.join("; ")),
    }
}

/// Minutes and seconds, as a person reads a time: `1:05`, `0:00`; hours
/// when there are any.
pub(crate) fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The running Klipp in plain words, for the person who asked whether it
/// is there (#626): the version, the open project and whether it is
/// saved, the timeline, the playhead, the media. No paths, sockets or
/// commit details beyond the build, which is what a bug report wants.
pub(crate) fn status_text(f: &StatusFacts) -> String {
    let b = f.build;
    let mut out = format!("Klipp {} is running (build {}", b.version, b.commit);
    if b.newer_build_on_disk() {
        out.push_str("; a newer build is installed, so restart Klipp to use it");
    }
    out.push_str(").\n");
    let name = f.project.and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string());
    out.push_str(&match (name, f.unsaved) {
        (Some(n), false) => format!("Project: {}, all changes saved.\n", quoted(&n)),
        (Some(n), true) => format!("Project: {}, with unsaved changes.\n", quoted(&n)),
        (None, false) => "Project: a new project, not saved yet.\n".to_string(),
        (None, true) => "Project: a new project with changes, not saved yet.\n".to_string(),
    });
    let seq = f.sequence;
    let fps = seq.fps.as_f64();
    let fps = if (fps - fps.round()).abs() < 1e-3 { format!("{}", fps.round()) } else { format!("{fps:.2}") };
    let clips = seq.tracks.iter().map(|t| t.clips.iter().filter(|c| !matches!(c.kind, klipp_timeline::ClipKind::Gap)).count()).sum::<usize>();
    let (pictures, sounds) = seq.tracks.iter().fold((0, 0), |(v, a), t| match t.kind {
        klipp_timeline::TrackKind::Audio => (v, a + 1),
        _ => (v + 1, a),
    });
    let contents = if clips == 0 {
        "no clips yet".to_string()
    } else {
        format!("{clips} clip{}, {} long", if clips == 1 { "" } else { "s" }, clock(seq.total_duration().to_secs_f64()))
    };
    out.push_str(&format!(
        "Timeline: {}, {}\u{00d7}{} at {fps} fps, {pictures} picture and {sounds} sound track{}, {contents}.\n",
        quoted(&seq.name),
        seq.resolution.width,
        seq.resolution.height,
        if sounds == 1 { "" } else { "s" },
    ));
    if f.sequences.len() > 1 {
        out.push_str(&sequences_text(f.sequences, f.active_sequence, seq.total_duration().to_secs_f64()));
        out.push('\n');
    }
    out.push_str(&format!("Playhead: {} at {}.\n", if f.playing { "playing" } else { "paused" }, clock(f.playhead_secs)));
    out.push_str(&match f.media {
        0 => "Media: none imported yet.".to_string(),
        1 => "Media: 1 file imported.".to_string(),
        n => format!("Media: {n} files imported."),
    });
    out
}

/// Which build is running, and whether a rebuild has replaced its
/// executable since it started, so a restart is due.
pub(crate) fn build_json() -> serde_json::Value {
    let build = klipp_core::build_info::get();
    json!({
        "label": build.label(),
        "version": build.version,
        "commit": build.commit,
        "dirty": build.dirty,
        "profile": build.profile,
        "exe": build.exe.as_ref().map(|p| p.display().to_string()),
        "built_at": build.built_at.map(klipp_core::build_info::format_utc),
        "built_at_unix": build.built_at,
        "newer_build_on_disk": build.newer_build_on_disk(),
    })
}

/// A bounded server-side wait keeps clients out of poll loops. Each read
/// releases shared locks before sleeping; the UI and other connections run.
pub(crate) fn wait_until<T>(wait: bool, timeout_ms: Option<u64>, mut read: impl FnMut() -> Result<(T, bool), String>) -> Result<(T, bool), String> {
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(3_600_000).min(86_400_000));
    let started = std::time::Instant::now();
    loop {
        let (value, finished) = read()?;
        if finished || !wait || started.elapsed() >= timeout {
            return Ok((value, finished));
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(timeout.saturating_sub(started.elapsed())));
    }
}

pub(crate) fn analysis_reply(ids: &[klipp_core::MediaId], wait: bool, timeout_ms: Option<u64>) -> Result<serde_json::Value, String> {
    let (items, finished) = wait_until(wait, timeout_ms, || {
        let items: Vec<_> = ids.iter().map(|id| klipp_analysis::service::status(*id).ok_or_else(|| format!("analysis {} is no longer available", id.0))).collect::<Result<_, _>>()?;
        let finished = items.iter().all(|s| matches!(s.state, klipp_analysis::service::State::Done | klipp_analysis::service::State::Failed | klipp_analysis::service::State::Cancelled));
        Ok((items, finished))
    })?;
    let succeeded = finished && items.iter().all(|s| s.state == klipp_analysis::service::State::Done);
    // What the models ran on, said once for the reply rather than again
    // in every item: the same lines on each of 20 files (#2492).
    let mut providers: Vec<String> = Vec::new();
    let items: Vec<serde_json::Value> = items
        .into_iter()
        .map(|s| {
            for line in &s.execution_providers {
                if !providers.contains(line) {
                    providers.push(line.clone());
                }
            }
            let mut v = json!(s);
            if let Some(o) = v.as_object_mut() {
                o.remove("execution_providers");
            }
            v
        })
        .collect();
    let mut reply = json!({ "items": items, "finished": finished, "succeeded": succeeded, "timed_out": wait && !finished });
    if !providers.is_empty() {
        reply["execution_providers"] = json!(providers);
    }
    Ok(reply)
}

/// How long a write waits for the UI to apply it before answering
/// "accepted" instead of "applied". The UI drains the inbox every frame
/// and is woken for it; anything longer than this means it is busy
/// (a modal, a load) and the client should not block on it.
pub(crate) const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

/// Hand `commands` to the UI as one envelope and wait for it to apply
/// them. The reply carries `applied` (false only if the UI did not answer
/// in time, in which case the commands are still queued), the document
/// `revision` after them, and the viewer `generation` from before them so
/// a frame newer than that reflects the change.
/// What the Undo menu calls an `apply` sent without a label: the first
/// op's name, and how many more came with it.
pub(crate) fn apply_label(ops: &[klipp_document::ops::Op]) -> String {
    let name = |op: &klipp_document::ops::Op| -> String {
        let tag = serde_json::to_value(op).ok().and_then(|v| v.get("op")?.as_str().map(|s| s.replace('_', " "))).unwrap_or_else(|| "edit".into());
        let mut chars = tag.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => tag,
        }
    };
    match ops {
        [] => "Apply".into(),
        [one] => name(one),
        [first, rest @ ..] => format!("{} and {} more", name(first), rest.len()),
    }
}

pub(crate) fn send_envelope(ctx: &ControlContext, commands: Vec<AppCommand>, batch: bool) -> Result<serde_json::Value, String> {
    send_full_envelope(ctx, commands, Vec::new(), batch, None, None)
}

/// The sound fixes a clip carries, as clean_audio puts them on: the
/// analysis reads the file, so what is already fixed must be known
/// separately (#505).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioFixes {
    pub(crate) wind: bool,
    pub(crate) hiss: bool,
    pub(crate) hum: bool,
}

impl AudioFixes {
    /// A clip that does not play the sound: it asks nothing of it.
    pub(crate) const ALL: Self = Self { wind: true, hiss: true, hum: true };

    pub(crate) fn on(effects: &[klipp_core::AudioEffectInstance]) -> Self {
        let param = |e: &klipp_core::AudioEffectInstance, k: &str| e.params.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
        let mut f = Self { wind: false, hiss: false, hum: false };
        for e in effects.iter().filter(|e| e.enabled && e.wet_dry > 0.0) {
            match e.effect_type.as_str() {
                // Voice isolation keeps the voice and nothing under it.
                "voice_isolation" => (f.wind, f.hiss) = (true, true),
                "denoiser" => {
                    if param(e, "wind").is_some_and(|w| w > 0.0) {
                        f.wind = true;
                    }
                    f.hiss = true;
                }
                "high_pass" => f.wind = true,
                "hum_removal" => f.hum = true,
                _ => {}
            }
        }
        f
    }

    /// Fixed wherever the sound is heard: in this and in `other`.
    pub(crate) fn and(self, other: Self) -> Self {
        Self { wind: self.wind && other.wind, hiss: self.hiss && other.hiss, hum: self.hum && other.hum }
    }

    /// Whether the fault `what` (as `AudioEnvelope::faults` words it) is
    /// one these fixes answer.
    pub(crate) fn covers(&self, what: &str) -> bool {
        (what.contains("hum") && self.hum)
            || ((what.contains("rumble") || what.contains("wind")) && self.wind)
            || ((what.contains("hiss") || what.contains("noise floor")) && self.hiss)
    }
}

/// grade_to_target's undo label: its steps carry it, and are folded
/// into one under it.
pub(crate) const GRADE_TO_TARGET: &str = "Grade To Target";

/// The undo steps of a tool that grades in rounds, measuring between
/// them: sent one by one under `label`, since each has to be applied to
/// be measured, and folded into one when the tool is done. A pass over
/// 31 shots was a hundred ctrl+z (#514).
pub(crate) struct UndoFold {
    pub(crate) label: &'static str,
    pub(crate) steps: usize,
    pub(crate) last_revision: Option<u64>,
}

impl UndoFold {
    pub(crate) fn new(label: &'static str) -> Self {
        Self { label, steps: 0, last_revision: None }
    }

    /// Apply `edits`, resolved at `revision`, as one of the pass's steps.
    pub(crate) fn send(&mut self, ctx: &ControlContext, edits: Vec<klipp_document::history::EditCommand>, revision: u64) -> Result<serde_json::Value, String> {
        let sent = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(self.label.into()))?;
        self.steps += 1;
        self.last_revision = sent.get("revision").and_then(|r| r.as_u64());
        Ok(sent)
    }

    /// Fold the pass's steps into one. The UI does it only if nothing
    /// else moved the document meanwhile, and never over an edit of the
    /// person's. The undo steps the pass is now.
    pub(crate) fn finish(&self, ctx: &ControlContext) -> usize {
        match (self.steps, self.last_revision) {
            (n @ (0 | 1), _) => n,
            (n, Some(rev)) => {
                let squash = klipp_core::AppCommand::SquashUndo { count: n, label: self.label.into(), only: self.label.into(), expected_revision: rev };
                let folded = send_full_envelope(ctx, vec![squash], Vec::new(), false, None, None).is_ok() && ctx.shared.revision() > rev;
                if folded { 1 } else { n }
            }
            (n, None) => n,
        }
    }
}

pub(crate) fn send_full_envelope(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    expected_revision: Option<u64>,
    label: Option<String>,
) -> Result<serde_json::Value, String> {
    send_full_envelope_for_project(ctx, commands, edits, batch, expected_revision, None, label)
}

pub(crate) fn send_full_envelope_for_project(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    expected_revision: Option<u64>,
    expected_project_identity: Option<u64>,
    label: Option<String>,
) -> Result<serde_json::Value, String> {
    send_with_reply(ctx, commands, edits, batch, EnvelopeReply { expected_revision, expected_project_identity, label, source_indices: None, expected_sources: Vec::new() })
}

pub(crate) struct EnvelopeReply {
    pub expected_revision: Option<u64>,
    pub expected_project_identity: Option<u64>,
    pub label: Option<String>,
    pub source_indices: Option<Vec<usize>>,
    /// The other sequences the edits copied from, as read (#2385).
    pub expected_sources: Vec<(klipp_core::SequenceId, u64)>,
}

pub(crate) fn send_with_reply(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    reply: EnvelopeReply,
) -> Result<serde_json::Value, String> {
    let EnvelopeReply { expected_revision, expected_project_identity, label, source_indices, expected_sources } = reply;
    // Read the generation *before* queueing, so any frame rendered in
    // response to this command is guaranteed to be newer than it.
    let generation = ctx
        .frame_buffer
        .lock()
        .map(|fb| fb.latest_generation())
        .unwrap_or(0);
    let count = commands.len() + edits.len();
    let (tx, rx) = std::sync::mpsc::channel::<ControlAck>();
    let background_job = if commands.iter().any(|c| matches!(c, AppCommand::ImportMedia { .. } | AppCommand::ImportMediaWithAnalysis { .. })) {
        Some("import_probe")
    } else if commands.iter().any(|c| matches!(c, AppCommand::ExportSequence { .. })) {
        Some("export")
    } else {
        None
    };
    let operation_id = operations::queued(background_job);
    ctx.commands
        .send(ControlEnvelope { commands, edits, batch, expected_revision, expected_project_identity, expected_sources, label, ack: Some(tx) })
        .map_err(|e| {
            operations::finished(&operation_id, ctx.shared.revision(), Some("app command channel closed".into()));
            format!("app command channel closed: {e}")
        })?;
    (ctx.wake_ui)();
    match rx.recv_timeout(ACK_TIMEOUT) {
        Ok(ack) => {
            let op_index = ack.refused_op.and_then(|at| source_indices.as_ref().and_then(|indices| indices.get(at)).copied());
            let ungated = ungated_notice_values(&ack);
            operations::finished_with_notices(&operation_id, ack.revision, ack.refused.clone(), op_index, ungated.clone());
            if let Some(why) = ack.refused {
                if source_indices.is_some() {
                    return Ok(json!({"accepted":false,"applied":false,"operation_id":operation_id,"revision":ack.revision,"op_index":op_index,"why":why}));
                }
                return Err(format!("not applied (operation {operation_id}): {why}"));
            }
            let mut reply = json!({
                "operation_id": operation_id,
                "applied": true,
                "accepted": true,
                "commands": count,
                "revision": ack.revision,
                "generation": generation,
            });
            // Ids the app's picture holds under other names once written
            // (#1256), for the caller to rename the paths it returns.
            if !ack.renamed.is_empty() {
                reply["renamed"] = json!(ack.renamed.iter().map(|(was, now)| [was.to_string(), now.to_string()]).collect::<Vec<_>>());
            }
            if !ack.ungated.is_empty() {
                reply["ungated"] = json!(ungated);
            }
            // A write that left its clip as it was (#2458): the rest applied.
            if let Some(why) = ack.picture_refused {
                reply["picture_write_refused"] = json!(why);
            }
            Ok(reply)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let pending_id = operation_id.clone();
            let shared = Arc::clone(&ctx.shared);
            std::thread::spawn(move || match rx.recv() {
                Ok(ack) => {
                    let op_index = ack.refused_op.and_then(|at| source_indices.as_ref().and_then(|indices| indices.get(at)).copied());
                    let ungated = ungated_notice_values(&ack);
                    operations::finished_with_notices(&pending_id, ack.revision, ack.refused, op_index, ungated);
                }
                Err(_) => operations::finished(&pending_id, shared.revision(), Some("UI acknowledgement channel closed".into())),
            });
            Ok(json!({
                "operation_id": operation_id,
                "applied": false,
                "accepted": true,
                "commands": count,
                "revision": ctx.shared.revision(),
                "generation": generation,
                "note": "queued for the UI's next frame; query operation_status for the final outcome",
            }))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            operations::finished(&operation_id, ctx.shared.revision(), Some("UI acknowledgement channel closed".into()));
            Err(format!("not applied (operation {operation_id}): UI acknowledgement channel closed"))
        }
    }
}

fn ungated_notice_values(ack: &ControlAck) -> Vec<serde_json::Value> {
    ack.ungated.iter().map(|n| json!({"path": crate::graph_wire::GraphPath::from(n.path.clone()), "label": n.label})).collect()
}
