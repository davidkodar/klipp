//! Control surface for a running Klipp instance.
//!
//! Starts a Unix-socket listener that accepts [`protocol::ControlCommand`]
//! requests, maps them onto the app's existing `AppCommand` channel and
//! shared state, and answers with JSON.
//!
//! The point is to make Klipp drivable without a GUI in the loop:
//! by an MCP server, by a script, by a test. Reads (status, clip list,
//! grade graph, screenshot, perf, log) go straight at the shared state;
//! writes are handed to the UI's command router (`KlippApp::send_commands`)
//! exactly as a button click is, so there is one code path for mutation
//! and the undo stack, autosave and engine caches all behave as they
//! would for a human.
//!
//! That routing is load-bearing, not a nicety. `AppCommand` is split
//! between two consumers: the UI handles colour, clip, media and most
//! editing commands itself and forwards only the remainder to the
//! engine. Writing straight to the engine's channel therefore made
//! transport work while every grade edit silently vanished.
//!
//! Enabled by default; `KLIPP_CONTROL=0` turns it off. The socket lives
//! in `$XDG_RUNTIME_DIR`, which is user-owned and mode 0700, so it is
//! reachable by the user running Klipp and nobody else.

pub mod protocol;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, RwLock};

use klipp_core::{AppCommand, ControlShared};
use klipp_document::control::{ControlAck, ControlEnvelope};
use protocol::{ControlCommand, Request, Response};
use serde_json::json;

/// Everything the server needs to read state and send commands. Built
/// in `main.rs` from the same handles the UI and engine already share.
#[derive(Clone)]
pub struct ControlContext {
    /// Into the UI's command router, *not* the engine's channel — see the
    /// module docs for why the difference matters.
    pub commands: crossbeam_channel::Sender<ControlEnvelope>,
    /// The document revision and the render queue, as the UI publishes
    /// them.
    pub shared: Arc<ControlShared>,
    /// Wakes the UI so it drains `commands`. egui repaints reactively; an
    /// idle window would otherwise sit on a queued command until the
    /// mouse next moved.
    pub wake_ui: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub sequence: Arc<RwLock<klipp_timeline::Sequence>>,
    pub media_pool: Arc<RwLock<klipp_media::MediaPool>>,
    pub frame_buffer: Arc<Mutex<klipp_engine::FrameBuffer>>,
    pub perf_ring: klipp_engine::perf::SharedPerfRing,
    /// Path of the app's debug log, for `log_tail`.
    pub log_path: std::path::PathBuf,
}

/// Start the control server unless `KLIPP_CONTROL=0`.
///
/// Returns the socket path on success. Never fatal: a failure here
/// leaves the app fully functional, just not remotely drivable, so it
/// logs and returns `None` rather than taking the process down.
pub fn spawn(ctx: ControlContext) -> Option<std::path::PathBuf> {
    if std::env::var("KLIPP_CONTROL").ok().as_deref() == Some("0") {
        tracing::info!("control socket disabled by KLIPP_CONTROL=0");
        return None;
    }
    let path = protocol::socket_path();
    // A socket file left behind by a crashed instance would make bind
    // fail with EADDRINUSE forever. Removing it is safe: if another
    // Klipp really is listening, this instance takes over and the old
    // one simply stops receiving — single-instance is the assumption,
    // and the alternative (refusing to start) is worse.
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("control socket unavailable at {}: {e}", path.display());
            return None;
        }
    };
    tracing::info!("control socket listening on {}", path.display());

    let thread_path = path.clone();
    std::thread::Builder::new()
        .name("klipp-control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let ctx = ctx.clone();
                        // One thread per connection. Connections are few
                        // (an MCP server, a script) and long-lived, so a
                        // pool would be ceremony for no gain.
                        let _ = std::thread::Builder::new()
                            .name("klipp-control-conn".into())
                            .spawn(move || serve_connection(s, ctx));
                    }
                    Err(e) => {
                        tracing::warn!("control socket accept failed: {e}");
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&thread_path);
        })
        .ok()?;
    Some(path)
}

fn serve_connection(stream: UnixStream, ctx: ControlContext) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("control connection clone failed: {e}");
            return;
        }
    });
    let mut writer = stream;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let id = req.id;
                match handle(&ctx, req.command, &mut None) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, e),
                }
            }
            // Parse failures answer on id 0 rather than dropping the
            // line: a client that sent malformed JSON should be told so,
            // not left waiting.
            Err(e) => Response::err(0, format!("bad request: {e}")),
        };
        let mut text = match serde_json::to_string(&response) {
            Ok(t) => t,
            Err(e) => format!(r#"{{"id":0,"ok":false,"error":"serialise failed: {e}"}}"#),
        };
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("{what} is not a uuid: {e}"))
}

fn clip_id(s: &str) -> Result<klipp_core::ClipId, String> {
    Ok(klipp_core::ClipId(parse_uuid(s, "clip_id")?))
}

/// A clip id that names a clip on the timeline.
///
/// Commands that address a clip used to parse the id and send the
/// command on, so an id that named nothing was answered `accepted` and
/// then dropped by the UI's `find_clip`. Checking here costs one read of
/// the sequence and turns a silent no-op into an error the caller can
/// act on.
fn existing_clip_id(
    ctx: &ControlContext,
    s: &str,
) -> Result<klipp_core::ClipId, String> {
    let want = clip_id(s)?;
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    if seq.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| c.id == want) {
        return Ok(want);
    }
    let mut have: Vec<String> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.id.0.to_string())
        .collect();
    have.truncate(8);
    Err(format!(
        "no clip with id {s} on the timeline. Clips are: {}",
        if have.is_empty() { "none".to_string() } else { have.join(", ") },
    ))
}

/// A node id that names a node in `clip`'s graph.
///
/// The same silent no-op as [`existing_clip_id`], and a worse one: node
/// ids are per clip, so passing the id of *another* clip's node — the
/// two clips' Correctors look alike, and both graphs start out as
/// Source, Corrector, Output — was accepted and changed nothing. The
/// grade read back as identity with no indication why.
///
/// Nodes inside a Compound's subgraph count: they are addressable, and
/// rejecting them here would break a legitimate write. So does
/// [`klipp_nodes::PHANTOM_CORRECTOR_ID`], which is not a node at all but
/// the request to materialise a Corrector on a clip that has none.
fn existing_node_id(
    ctx: &ControlContext,
    clip: klipp_core::ClipId,
    s: &str,
    what: &str,
) -> Result<uuid::Uuid, String> {
    let want = parse_uuid(s, what)?;
    if want == klipp_nodes::PHANTOM_CORRECTOR_ID {
        return Ok(want);
    }
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let graph = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == clip)
        .map(|c| &c.node_graph)
        .ok_or_else(|| format!("no clip with id {} on the timeline", clip.0))?;
    if find_node(graph, want) {
        return Ok(want);
    }
    Err(format!(
        "clip {} has no node {s}. Its nodes are: {}. \
         Node ids belong to one clip — another clip's Corrector will not do.",
        clip.0,
        node_summary(graph).join(", "),
    ))
}

/// Whether `id` is anywhere in `graph`, including inside a Compound.
fn find_node(graph: &klipp_nodes::NodeGraph, id: uuid::Uuid) -> bool {
    graph.nodes.iter().any(|n| {
        n.id == id
            || n.params
                .compound_subgraph
                .as_ref()
                .is_some_and(|sub| find_node(sub, id))
    })
}

/// `kind id` for each node, so the error says what the caller could have
/// meant rather than only what they got wrong.
fn node_summary(graph: &klipp_nodes::NodeGraph) -> Vec<String> {
    graph
        .nodes
        .iter()
        .map(|n| format!("{:?} {}", n.kind, n.id))
        .collect()
}

/// Every `LogFormat` a client may name, in the spelling the wire uses.
pub const LOG_FORMAT_NAMES: &[&str] = &[
    "Srgb", "Linear", "LogC3", "LogC4", "SLog2", "SLog3", "CanonLog", "CanonLog3",
    "VLog", "NLog", "FLog", "DJIDLog", "GoProFlat", "AppleLog", "LeicaLLog",
    "RedLog3G10", "BmdFilmGen5", "HLG", "PQ",
];

fn parse_log_format(name: &str) -> Result<klipp_core::LogFormat, String> {
    serde_json::from_value(json!(name)).map_err(|_| {
        format!("unknown log format {name:?}; expected one of {}", LOG_FORMAT_NAMES.join(", "))
    })
}

/// The timeline as text: what a model reads before it plans. Tracks top
/// first; every clip with its range in seconds and frames at the
/// sequence's rate; the clip's kind and source; its grade and effects in
/// a word; the markers. Ids are there because an operation needs them,
/// at the end of the line where they cost the least attention.
fn timeline_text(
    seq: &klipp_timeline::Sequence,
    pool: &klipp_media::MediaPool,
    playhead_secs: Option<f64>,
    revision: u64,
) -> String {
    use std::fmt::Write as _;
    let fps = seq.fps.as_f64();
    let frames = |t: klipp_core::Timecode| (t.to_secs_f64() * fps).round() as i64;
    let range = |a: klipp_core::Timecode, b: klipp_core::Timecode| {
        format!("{:.2}-{:.2} s | {}-{} f", a.to_secs_f64(), b.to_secs_f64(), frames(a), frames(b))
    };
    let mut out = String::new();
    let dur = seq.total_duration();
    let _ = writeln!(
        out,
        "Sequence \"{}\"  {}x{}  {:.3} fps  duration {:.2} s ({} f)  revision {}",
        seq.name, seq.resolution.width, seq.resolution.height, fps, dur.to_secs_f64(), frames(dur), revision,
    );
    if let Some(p) = playhead_secs {
        let _ = writeln!(out, "Playhead {:.2} s ({} f)", p, (p * fps).round() as i64);
    }
    let _ = writeln!(out, "Tracks, top first. Times are seconds and frames at {:.3} fps.", fps);
    for (i, t) in seq.tracks.iter().enumerate() {
        let mut head = format!("\n[{i}] {:?} \"{}\"", t.kind, t.name);
        if (t.opacity - 1.0).abs() > 1e-6 {
            let _ = write!(head, "  opacity {:.2}", t.opacity);
        }
        if t.locked {
            head.push_str("  locked");
        }
        if t.clips.is_empty() {
            head.push_str("  (empty)");
        }
        let _ = writeln!(out, "{head}");
        for c in &t.clips {
            let what = match &c.kind {
                klipp_timeline::ClipKind::Media { media_id } => match pool.get(media_id) {
                    Some(m) => {
                        let mut line = format!("media {}  src {:.2}-{:.2} s", m.filename, c.source_in.to_secs_f64(), c.source_out.to_secs_f64());
                        // What analysis knows about this part of the file.
                        let known = klipp_analysis::report::MediaAnalysis::load(&m.path)
                            .clip_summary_with(c.source_in.to_secs_f64(), c.source_out.to_secs_f64(), t.kind != klipp_timeline::TrackKind::Audio);
                        if !known.is_empty() {
                            let _ = write!(line, "  [{known}]");
                        }
                        line
                    }
                    None => "media (missing from pool)".to_string(),
                },
                klipp_timeline::ClipKind::Title(td) => {
                    let text = td.text();
                    let one_line: String = text.lines().collect::<Vec<_>>().join(" / ");
                    let shown: String = one_line.chars().take(60).collect();
                    format!("title \"{}{}\"", shown, if one_line.chars().count() > 60 { "…" } else { "" })
                }
                klipp_timeline::ClipKind::Generator(g) => format!("generator {:?}", g).chars().take(48).collect(),
                klipp_timeline::ClipKind::AdjustmentLayer => "adjustment layer".to_string(),
                klipp_timeline::ClipKind::Gap => "gap".to_string(),
                klipp_timeline::ClipKind::Sequence { .. } => "nested sequence".to_string(),
            };
            let mut line = format!("  [{}]  {what}", range(c.timeline_in, c.timeline_out));
            if !c.enabled {
                line.push_str("  DISABLED");
            }
            if (c.speed - 1.0).abs() > 1e-6 {
                let _ = write!(line, "  speed {:.2}", c.speed);
            }
            if !matches!(c.blend_mode, klipp_core::BlendMode::Normal) {
                let _ = write!(line, "  blend {:?}", c.blend_mode);
            }
            let grade = if c.node_graph.is_identity() { "none".to_string() } else { format!("{} nodes", c.node_graph.nodes.len()) };
            let _ = write!(line, "  grade {grade}");
            if !c.builtin_effects.is_empty() {
                let names: Vec<String> = c
                    .builtin_effects
                    .iter()
                    .map(|e| {
                        let d = format!("{e:?}");
                        d.split(['{', '(', ' ']).next().unwrap_or("").to_string()
                    })
                    .collect();
                let _ = write!(line, "  fx {}", names.join(","));
            }
            if !c.effects.is_empty() {
                let _ = write!(line, "  plugins {}", c.effects.len());
            }
            if c.matte_source.is_some() {
                line.push_str("  matted");
            }
            if c.consumed_as_matte {
                line.push_str("  is-a-matte");
            }
            let _ = write!(line, "  id {}", c.id.0);
            let _ = writeln!(out, "{line}");
        }
    }
    if !seq.markers.is_empty() {
        let _ = writeln!(out, "\nMarkers");
        for m in &seq.markers {
            let at = match m.duration {
                Some(d) => format!("{:.2}-{:.2} s", m.position.to_secs_f64(), (m.position + d).to_secs_f64()),
                None => format!("{:.2} s", m.position.to_secs_f64()),
            };
            let _ = writeln!(out, "  {at}  \"{}\"  {:?}", m.label, m.color);
        }
    }
    out
}

/// `text` within `max_chars`, cut on a line boundary and saying so, and
/// whether it was cut.
fn cut_to_budget(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let mut kept = String::new();
    for line in text.lines() {
        if kept.chars().count() + line.chars().count() + 1 > max_chars.saturating_sub(120) {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept.push_str(&format!(
        "… cut at {max_chars} chars of {}; ask again with a larger max_chars, or list_clips for the rest.\n",
        text.chars().count()
    ));
    (kept, true)
}

/// `ops` resolved against the document as the socket sees it, with the
/// revision that view had.
fn resolve_plan(ctx: &ControlContext, ops: &[klipp_document::ops::Op]) -> Result<(klipp_document::ops::Plan, u64), String> {
    if ops.is_empty() {
        return Err("no ops given".into());
    }
    let revision = ctx.shared.revision();
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
}

fn project_status(ctx: &ControlContext) -> Result<klipp_core::ProjectStatus, String> {
    Ok(ctx.shared.project.lock().map_err(|_| "project status lock poisoned")?.clone())
}

/// Wait for the UI to report a save or an open: its generation moves
/// when one ends, well or badly.
fn wait_for_project_event(ctx: &ControlContext, before: u64, timeout_ms: u64) -> Result<klipp_core::ProjectStatus, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let now = project_status(ctx)?;
        if now.generation != before {
            return Ok(now);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("the app did not report back within {timeout_ms} ms; see the log"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A project is a folder whose name ends in `.klipp`.
fn with_klipp_extension(mut path: std::path::PathBuf) -> std::path::PathBuf {
    if path.extension().and_then(|e| e.to_str()) != Some("klipp") {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        path.set_file_name(format!("{name}.klipp"));
    }
    path
}

/// The transcripts analysis has made, for the resolver's `captions`.
struct AnalysedTranscripts;

impl klipp_document::ops::Transcripts for AnalysedTranscripts {
    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<klipp_document::ops::SpokenWord>> {
        let t = klipp_analysis::report::MediaAnalysis::load(&item.path).transcript?;
        Some(t.words.into_iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text }).collect())
    }
}

/// A media item's file and duration, by id.
fn media_path_and_duration(ctx: &ControlContext, media_id: &str) -> Result<(std::path::PathBuf, f64), String> {
    let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
    if item.offline || item.kind != klipp_media::MediaItemKind::File {
        return Err(format!("{} has no file to read", item.filename));
    }
    Ok((item.path.clone(), item.duration.to_secs_f64()))
}

/// An absolute output path the caller gave, or one in the temp dir.
fn temp_output(path: Option<String>, stem: &str, ext: &str) -> Result<std::path::PathBuf, String> {
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
fn fps_from_f64(f: f64) -> klipp_core::Fps {
    for (num, den) in [(24000u32, 1001u32), (30000, 1001), (60000, 1001), (48000, 1001)] {
        if (f - num as f64 / den as f64).abs() < 0.005 {
            return klipp_core::Fps::new(num, den);
        }
    }
    klipp_core::Fps::new((f * 1000.0).round().max(1.0) as u32, 1000)
}

/// A clip's kind as a client reads it.
fn clip_kind_name(c: &klipp_timeline::Clip) -> &'static str {
    match &c.kind {
        klipp_timeline::ClipKind::Media { .. } => "media",
        klipp_timeline::ClipKind::Title(_) => "title",
        klipp_timeline::ClipKind::Generator(_) => "generator",
        klipp_timeline::ClipKind::AdjustmentLayer => "adjustment",
        klipp_timeline::ClipKind::Gap => "gap",
        klipp_timeline::ClipKind::Sequence { .. } => "sequence",
    }
}

fn log_format_json(m: &klipp_media::MediaItem) -> serde_json::Value {
    json!({
        // What the file's metadata says, and what Klipp decodes it as —
        // the user's choice when they made one, the detection otherwise.
        "detected": format!("{:?}", m.detected_log_format),
        "effective": format!("{:?}", m.effective_log_format()),
    })
}

/// Which build is running, and whether a rebuild has replaced its
/// executable since it started, so a restart is due.
fn build_json() -> serde_json::Value {
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

/// How long a write waits for the UI to apply it before answering
/// "accepted" instead of "applied". The UI drains the inbox every frame
/// and is woken for it; anything longer than this means it is busy
/// (a modal, a load) and the client should not block on it.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

/// Hand `commands` to the UI as one envelope and wait for it to apply
/// them. The reply carries `applied` (false only if the UI did not answer
/// in time, in which case the commands are still queued), the document
/// `revision` after them, and the viewer `generation` from before them so
/// a frame newer than that reflects the change.
fn send_envelope(ctx: &ControlContext, commands: Vec<AppCommand>, batch: bool) -> Result<serde_json::Value, String> {
    send_full_envelope(ctx, commands, Vec::new(), batch, None, None)
}

fn send_full_envelope(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    expected_revision: Option<u64>,
    label: Option<String>,
) -> Result<serde_json::Value, String> {
    // Read the generation *before* queueing, so any frame rendered in
    // response to this command is guaranteed to be newer than it.
    let generation = ctx
        .frame_buffer
        .lock()
        .map(|fb| fb.latest_generation())
        .unwrap_or(0);
    let count = commands.len() + edits.len();
    let (tx, rx) = std::sync::mpsc::channel::<ControlAck>();
    ctx.commands
        .send(ControlEnvelope { commands, edits, batch, expected_revision, label, ack: Some(tx) })
        .map_err(|e| format!("app command channel closed: {e}"))?;
    (ctx.wake_ui)();
    match rx.recv_timeout(ACK_TIMEOUT) {
        Ok(ack) => {
            if let Some(why) = ack.refused {
                return Err(format!("not applied: {why}"));
            }
            Ok(json!({
                "applied": true,
                "accepted": true,
                "commands": count,
                "revision": ack.revision,
                "generation": generation,
            }))
        }
        Err(_) => Ok(json!({
            "applied": false,
            "accepted": true,
            "commands": count,
            "revision": ctx.shared.revision(),
            "generation": generation,
            "note": "queued for the UI's next frame; it did not answer within the timeout",
        })),
    }
}

/// `collect` is `Some` inside a batch: writes are gathered there instead
/// of being sent, so the batch goes to the UI whole or not at all.
fn handle(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Vec<AppCommand>>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();
    let mut send = |c: AppCommand| -> Result<serde_json::Value, String> {
        if let Some(buf) = collect.as_mut() {
            buf.push(c);
            return Ok(json!({ "queued": true }));
        }
        send_envelope(ctx, vec![c], false)
    };
    // A read inside a batch would answer before the writes it follows
    // are applied, which is a lie; refuse it.
    let read_in_batch = |what: &str| -> Result<serde_json::Value, String> {
        Err(format!("{what} cannot be part of a batch; a batch is for writes"))
    };

    match cmd {
        ControlCommand::Ping => Ok(json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "build": build_json(),
        })),

        ControlCommand::Status => {
            if in_batch {
                return read_in_batch("status");
            }
            let project = project_status(ctx)?;
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            let perf = ctx.perf_ring.lock().ok().and_then(|r| r.latest().cloned());
            let fps = seq.fps.as_f64();
            let presented = fb.latest_presented().map(|t| t.to_secs_f64());
            Ok(json!({
                "build": build_json(),
                "revision": ctx.shared.revision(),
                "project": {
                    "path": project.path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "unsaved_changes": project.dirty,
                },
                "tracks": seq.tracks.len(),
                "clips": seq.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
                "fps": fps,
                "resolution": { "width": seq.resolution.width, "height": seq.resolution.height },
                "duration_secs": seq.total_duration().to_secs_f64(),
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

        ControlCommand::ListClips => {
            if in_batch {
                return read_in_batch("list_clips");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let fps = seq.fps.as_f64();
            let tracks: Vec<serde_json::Value> = seq
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    json!({
                        "index": i,
                        "opacity": t.opacity,
                        "kind": format!("{:?}", t.kind),
                        "name": t.name,
                        "locked": t.locked,
                        "clips": t.clips.iter().map(|c| json!({
                            "clip_id": c.id.0.to_string(),
                            "kind": clip_kind_name(c),
                            "enabled": c.enabled,
                            "timeline_in_secs": c.timeline_in.to_secs_f64(),
                            "timeline_out_secs": c.timeline_out.to_secs_f64(),
                            "timeline_in_frame": (c.timeline_in.to_secs_f64() * fps).round() as i64,
                            "timeline_out_frame": (c.timeline_out.to_secs_f64() * fps).round() as i64,
                            "source_in_secs": c.source_in.to_secs_f64(),
                            "source_out_secs": c.source_out.to_secs_f64(),
                            "speed": c.speed,
                            "blend_mode": format!("{:?}", c.blend_mode),
                            "grade_is_identity": c.node_graph.is_identity(),
                            "grade_nodes": c.node_graph.nodes.len(),
                            "media": c.media_id()
                                .and_then(|id| pool.get(&id))
                                .map(|m| json!({
                                    "media_id": m.id.0.to_string(),
                                    "filename": m.filename,
                                    "path": m.path.to_string_lossy(),
                                    "log_format": log_format_json(m),
                                })),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(json!({ "fps": fps, "revision": ctx.shared.revision(), "tracks": tracks }))
        }

        ControlCommand::Analyze { media_id, download_models } => {
            if in_batch {
                return read_in_batch("analyze");
            }
            let downloaded = if download_models { klipp_analysis::download_models()? } else { Vec::new() };
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let items: Vec<&klipp_media::MediaItem> = match media_id {
                Some(id) => {
                    let id = klipp_core::MediaId(id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
                    vec![pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?]
                }
                None => pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File && !m.offline).collect(),
            };
            let statuses: Vec<klipp_analysis::service::Status> = items.into_iter().map(klipp_analysis::service::request).collect();
            Ok(json!({ "items": statuses, "models_downloaded": downloaded }))
        }

        ControlCommand::SearchTranscripts { query, max_hits } => {
            if in_batch {
                return read_in_batch("search_transcripts");
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let mut hits = Vec::new();
            let max = max_hits.unwrap_or(50);
            for m in pool.all() {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(t) = report.transcript.as_ref() else { continue };
                for seg in t.search(&query) {
                    hits.push(json!({
                        "media_id": m.id.0.to_string(),
                        "filename": m.filename,
                        "start_secs": seg.start_secs,
                        "end_secs": seg.end_secs,
                        "text": seg.text,
                    }));
                    if hits.len() >= max {
                        break;
                    }
                }
                if hits.len() >= max {
                    break;
                }
            }
            Ok(json!({ "query": query, "hits": hits }))
        }

        ControlCommand::AnalysisStatus => {
            if in_batch {
                return read_in_batch("analysis_status");
            }
            Ok(json!({ "items": klipp_analysis::service::all() }))
        }

        ControlCommand::MediaAnalysis { media_id, max_chars } => {
            if in_batch {
                return read_in_batch("media_analysis");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let status = klipp_analysis::service::status(id).map(|s| s.state);
            let full = format!("{}  {:.2} s\n{}", item.filename, item.duration.to_secs_f64(), report.text(max_chars.unwrap_or(8000)));
            let (text, cut) = cut_to_budget(&full, max_chars.unwrap_or(8000));
            Ok(json!({
                "text": text,
                "cut": cut,
                "analysed": !report.is_empty(),
                "state": status,
                "shots": report.shots,
                "audio": report.audio,
                "transcript": report.transcript,
                "motion": report.motion,
                "framing": report.framing,
            }))
        }

        ControlCommand::FindShots { framing, min_people, object, motion, with_speech, min_secs, max_hits } => {
            if in_batch {
                return read_in_batch("find_shots");
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let max = max_hits.unwrap_or(50);
            let mut hits = Vec::new();
            let object = object.map(|o| o.to_lowercase());
            let framing = framing.map(|f| f.to_lowercase());
            let motion = motion.map(|m| m.to_lowercase());
            'items: for m in pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File) {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(shots) = report.shots.as_ref() else { continue };
                for (i, shot) in shots.shots.iter().enumerate() {
                    let (a, b) = (shot.start_secs, shot.end_secs);
                    if min_secs.is_some_and(|s| b - a < s) {
                        continue;
                    }
                    let fr = report.framing.as_ref().and_then(|f| f.shots.iter().find(|s| (s.start_secs - a).abs() < 0.05));
                    if let Some(want) = &framing {
                        if !fr.is_some_and(|f| f.framing == *want) {
                            continue;
                        }
                    }
                    if let Some(n) = min_people {
                        if !fr.is_some_and(|f| f.people >= n) {
                            continue;
                        }
                    }
                    if let Some(o) = &object {
                        if !fr.is_some_and(|f| f.objects.iter().any(|x| x.to_lowercase() == *o)) {
                            continue;
                        }
                    }
                    let motion_class = report.motion.as_ref().and_then(|mo| mo.class_within(a, b));
                    if let Some(want) = &motion {
                        if motion_class != Some(want.as_str()) {
                            continue;
                        }
                    }
                    let words = report.transcript.as_ref().map(|t| t.words_within(a, b).len()).unwrap_or(0);
                    if let Some(want) = with_speech {
                        if (words > 0) != want {
                            continue;
                        }
                    }
                    hits.push(json!({
                        "media_id": m.id.0.to_string(),
                        "filename": m.filename,
                        "shot": i + 1,
                        "start_secs": a,
                        "end_secs": b,
                        "framing": fr.map(|f| f.framing.clone()),
                        "people": fr.map(|f| f.people),
                        "position": fr.map(|f| f.position.clone()),
                        "objects": fr.map(|f| f.objects.clone()),
                        "motion": motion_class,
                        "words": words,
                    }));
                    if hits.len() >= max {
                        break 'items;
                    }
                }
            }
            Ok(json!({ "hits": hits }))
        }

        ControlCommand::ContactSheet { media_id, from_secs, to_secs, frames, cols, tile_width, path } => {
            if in_batch {
                return read_in_batch("contact_sheet");
            }
            let (media_path, duration) = media_path_and_duration(ctx, &media_id)?;
            let out = temp_output(path, "klipp-sheet", "jpg")?;
            let from = from_secs.unwrap_or(0.0).clamp(0.0, duration);
            let to = to_secs.unwrap_or(duration).clamp(from, duration);
            let sheet = klipp_analysis::sheet::contact_sheet(
                std::path::Path::new("ffmpeg"), &media_path, from, to, frames.unwrap_or(12), cols.unwrap_or(4), tile_width.unwrap_or(240), &out,
            )?;
            Ok(serde_json::to_value(sheet).map_err(|e| e.to_string())?)
        }

        ControlCommand::AudioExcerpt { media_id, from_secs, to_secs, path } => {
            if in_batch {
                return read_in_batch("audio_excerpt");
            }
            let (media_path, duration) = media_path_and_duration(ctx, &media_id)?;
            let out = temp_output(path, "klipp-excerpt", "wav")?;
            let from = from_secs.clamp(0.0, duration);
            let to = to_secs.clamp(from, duration);
            let p = klipp_analysis::sheet::audio_excerpt(std::path::Path::new("ffmpeg"), &media_path, from, to, &out)?;
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            Ok(json!({ "path": p.to_string_lossy(), "from_secs": from, "to_secs": to, "bytes": bytes, "format": "wav 16 kHz mono 16-bit" }))
        }

        ControlCommand::Timeline { max_chars } => {
            if in_batch {
                return read_in_batch("timeline");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let presented = ctx.frame_buffer.lock().ok().and_then(|fb| fb.latest_presented()).map(|t| t.to_secs_f64());
            let full = timeline_text(&seq, &pool, presented, ctx.shared.revision());
            let (text, cut) = cut_to_budget(&full, max_chars.unwrap_or(12_000));
            Ok(json!({ "text": text, "cut": cut, "chars": full.len(), "revision": ctx.shared.revision() }))
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
                })).collect::<Vec<_>>(),
            }))
        }

        ControlCommand::ImportMedia { path } => {
            let p = std::path::PathBuf::from(&path);
            if !p.exists() {
                return Err(format!("file not found: {path}"));
            }
            send(AppCommand::ImportMedia { path: p })
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
        ControlCommand::SetAppendAudio { enabled } => {
            send(AppCommand::SetAppendBringsAudio(enabled))
        }
        ControlCommand::SelectClip { clip_id: c } => {
            let id = match c {
                Some(s) => Some(existing_clip_id(ctx, &s)?),
                None => None,
            };
            send(AppCommand::SelectClip(id))
        }

        ControlCommand::GetGrade { clip_id: c } => {
            let want = clip_id(&c)?;
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let clip = seq
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|cl| cl.id == want)
                .ok_or_else(|| format!("no clip with id {c}"))?;
            Ok(json!({
                "clip_id": c,
                "is_identity": clip.node_graph.is_identity(),
                "graph": serde_json::to_value(&clip.node_graph)
                    .map_err(|e| format!("serialising graph: {e}"))?,
            }))
        }

        ControlCommand::AddNode { clip_id: c, kind, x, y } => {
            send(AppCommand::AddNode { clip_id: existing_clip_id(ctx, &c)?, kind, position: (x, y) })
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
            send(AppCommand::ConnectNodes {
                clip_id: clip,
                from: existing_node_id(ctx, clip, &from, "from")?,
                from_port,
                to: existing_node_id(ctx, clip, &to, "to")?,
                to_port,
            })
        }
        ControlCommand::SetNodeParam { clip_id: c, node_id, param, value } => {
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
            send(AppCommand::SetNodeParam { clip_id: clip, node_id, param, value })
        }
        ControlCommand::ToggleNodeEnabled { clip_id: c, node_id } => {
            let clip = existing_clip_id(ctx, &c)?;
            send(AppCommand::ToggleNodeEnabled {
                clip_id: clip,
                node_id: existing_node_id(ctx, clip, &node_id, "node_id")?,
            })
        }

        ControlCommand::SetLogFormat { clip_id: c, format } => {
            let log_format = parse_log_format(&format)?;
            send(AppCommand::SetClipLogFormat { clip_id: existing_clip_id(ctx, &c)?, log_format })
        }

        ControlCommand::Screenshot { path, max_width, min_generation, timeout_ms } => {
            if in_batch {
                return read_in_batch("screenshot");
            }
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
            let (data, width, height, generation) = loop {
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
            Ok(json!({
                "path": out.to_string_lossy(),
                "width": width,
                "height": height,
                "generation": generation,
            }))
        }

        ControlCommand::Plan { ops } => {
            if in_batch {
                return read_in_batch("plan");
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            Ok(json!({ "text": plan.text(), "steps": plan.report(), "duration_secs": plan.duration_secs, "revision": revision, "applied": false }))
        }

        ControlCommand::Apply { ops, label } => {
            if in_batch {
                return Err("apply is its own batch; send it on its own".into());
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), label)?;
            v["text"] = json!(plan.text());
            v["steps"] = json!(plan.report());
            v["duration_secs"] = json!(plan.duration_secs);
            Ok(v)
        }

        ControlCommand::Brief => {
            if in_batch {
                return read_in_batch("brief");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            Ok(match &seq.brief {
                Some(b) => json!({ "brief": b, "text": b.text() }),
                None => json!({ "brief": null, "text": "There is no brief. Set one with set_brief: what the piece is about, its length, platform, loudness, whether it is captioned, and the cast with names and roles as they should appear." }),
            })
        }

        ControlCommand::SetBrief { brief } => {
            if in_batch {
                return Err("set_brief is its own undo step; send it on its own".into());
            }
            if let Some(t) = brief.target_secs {
                if !t.is_finite() || t <= 0.0 {
                    return Err(format!("a length of {t} s is not a length"));
                }
            }
            if let Some(l) = brief.loudness_lufs {
                if !(-40.0..=-5.0).contains(&l) {
                    return Err(format!("{l} LUFS is not a delivery loudness; the web is about -14, broadcast -23"));
                }
            }
            if brief.width.is_some() != brief.height.is_some() {
                return Err("give both width and height, or neither".into());
            }
            if let Some(dup) = brief.cast.iter().enumerate().find(|(i, c)| brief.cast[..*i].iter().any(|d| d.name.trim().eq_ignore_ascii_case(c.name.trim()))) {
                return Err(format!("{} is in the cast twice", dup.1.name));
            }
            if brief.cast.iter().any(|c| c.name.trim().is_empty()) {
                return Err("a cast member needs a name".into());
            }
            let old = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone();
            let text = brief.text();
            let edit = klipp_document::history::EditCommand::SetBrief { old: old.map(Box::new), new: Some(Box::new(brief)) };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Set Brief".into()))?;
            v["text"] = json!(text);
            Ok(v)
        }

        ControlCommand::Check => {
            if in_batch {
                return read_in_batch("check");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let report = klipp_document::qc::check(&seq, &pool, &AnalysedTranscripts);
            Ok(json!({ "text": report.text(), "errors": report.errors, "warnings": report.warnings, "findings": report.findings, "duration_secs": report.duration_secs, "revision": ctx.shared.revision() }))
        }

        ControlCommand::Undo => send(AppCommand::Undo),
        ControlCommand::Redo => send(AppCommand::Redo),

        ControlCommand::Batch { commands } => {
            if in_batch {
                return Err("a batch cannot contain a batch".into());
            }
            if commands.is_empty() {
                return Err("an empty batch".into());
            }
            let mut buf = Some(Vec::with_capacity(commands.len()));
            for (i, c) in commands.into_iter().enumerate() {
                // Every command checked and translated before any is sent:
                // a bad one refuses the batch whole.
                handle(ctx, c, &mut buf).map_err(|e| format!("batch command {i}: {e}"))?;
            }
            let cmds = buf.take().unwrap_or_default();
            send_envelope(ctx, cmds, true)
        }

        ControlCommand::Export { path, width, height, fps, crf, target_lufs } => {
            let out = std::path::PathBuf::from(&path);
            if !out.is_absolute() {
                return Err(format!("export path must be absolute (got {path:?})"));
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
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
            drop(pool);
            drop(seq);
            let (width, height) = match (width, height, brief.as_ref()) {
                (None, None, Some(b)) => (b.width, b.height),
                _ => (width, height),
            };
            let target_lufs = target_lufs.or(brief.as_ref().and_then(|b| b.loudness_lufs));
            let format = match out.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
                Some("mp4") | None => klipp_core::ExportFormat::Mp4,
                Some("mov") => klipp_core::ExportFormat::Mov,
                Some("mkv") => klipp_core::ExportFormat::Mkv,
                Some("webm") => klipp_core::ExportFormat::WebM,
                Some(other) => return Err(format!("unsupported container .{other}; use mp4, mov, mkv or webm")),
            };
            let mut settings = klipp_core::ExportSettings { output_path: out, format, ..Default::default() };
            if let (Some(w), Some(h)) = (width, height) {
                settings.resolution = Some(klipp_core::Resolution { width: w, height: h });
            }
            if let Some(f) = fps {
                settings.fps = Some(fps_from_f64(f));
            }
            if let Some(c) = crf {
                settings.crf = c;
            }
            if target_lufs.is_some() {
                settings.target_lufs = target_lufs;
            }
            send(AppCommand::ExportSequence { settings })
        }

        ControlCommand::ExportStatus => {
            if in_batch {
                return read_in_batch("export_status");
            }
            let jobs = ctx.shared.exports.lock().map_err(|_| "export status lock poisoned")?;
            Ok(json!({ "jobs": *jobs }))
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

        ControlCommand::FrameAt { secs, path, max_width, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_at");
            }
            let (was_at, fps) = {
                let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                (fb.latest_presented(), seq.fps)
            };
            let target = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
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
            let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
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
                    if let Some(t) = was_at {
                        let _ = send_envelope(ctx, vec![AppCommand::SeekTo(t)], false);
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
            let shot = handle(
                ctx,
                ControlCommand::Screenshot { path, max_width, min_generation: Some(generation), timeout_ms },
                &mut None,
            );
            // Back where the playhead was, whatever the capture did.
            if let Some(t) = was_at {
                let _ = send_envelope(ctx, vec![AppCommand::SeekTo(t)], false);
            }
            let mut v = shot?;
            v["secs"] = json!(secs);
            v["frame"] = json!((secs * fps.as_f64()).round() as i64);
            Ok(v)
        }

        ControlCommand::Perf => {
            if in_batch {
                return read_in_batch("perf");
            }
            let ring = ctx.perf_ring.lock().map_err(|_| "perf ring lock poisoned")?;
            let latest = ring.latest().cloned();
            let avg = ring.averages(60);
            Ok(json!({
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
    }
}

/// Nearest-neighbour downscale. Good enough for eyeballing a frame and
/// dependency-free; anything doing measurement should take the
/// full-resolution PNG instead.
fn downscale_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_width: u32,
) -> (Vec<u8>, u32, u32) {
    let tw = target_width.max(1);
    let th = ((height as u64 * tw as u64) / width.max(1) as u64).max(1) as u32;
    let mut out = Vec::with_capacity((tw as usize) * (th as usize) * 4);
    for y in 0..th {
        let sy = ((y as u64 * height as u64) / th as u64).min(height as u64 - 1) as usize;
        for x in 0..tw {
            let sx = ((x as u64 * width as u64) / tw as u64).min(width as u64 - 1) as usize;
            let o = (sy * width as usize + sx) * 4;
            out.extend_from_slice(&rgba[o..o + 4]);
        }
    }
    (out, tw, th)
}

fn write_png(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // A frame someone is waiting to look at: the default compression
    // spent several hundred milliseconds on a 1080p frame to save a
    // file that is read once and thrown away.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
    writer.write_image_data(rgba).map_err(|e| format!("png data: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive on purpose: a variant added to `LogFormat` stops this
    /// compiling until it is given a name in `LOG_FORMAT_NAMES` too.
    fn wire_name(f: klipp_core::LogFormat) -> &'static str {
        use klipp_core::LogFormat as L;
        match f {
            L::Srgb => "Srgb", L::Linear => "Linear", L::LogC3 => "LogC3", L::LogC4 => "LogC4",
            L::SLog2 => "SLog2", L::SLog3 => "SLog3", L::CanonLog => "CanonLog",
            L::CanonLog3 => "CanonLog3", L::VLog => "VLog", L::NLog => "NLog", L::FLog => "FLog",
            L::DJIDLog => "DJIDLog", L::GoProFlat => "GoProFlat", L::AppleLog => "AppleLog",
            L::LeicaLLog => "LeicaLLog", L::RedLog3G10 => "RedLog3G10",
            L::BmdFilmGen5 => "BmdFilmGen5", L::HLG => "HLG", L::PQ => "PQ",
        }
    }

    #[test]
    fn log_format_names_cover_the_enum_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for name in LOG_FORMAT_NAMES {
            let f = parse_log_format(name).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(wire_name(f), *name);
            assert!(seen.insert(*name), "{name} listed twice");
        }
        assert_eq!(seen.len(), 19, "wire_name has 19 arms; every one must be listed");
        assert!(parse_log_format("slog3").is_err(), "names are case-sensitive");
    }
}
