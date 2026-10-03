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
mod operations;
mod cut_prep;
mod jobs;
mod json_path;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, RwLock};

use klipp_core::{AppCommand, ControlShared};
use klipp_document::control::{ControlAck, ControlEnvelope};
use protocol::{ControlCommand, Listen, Request, Response};
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
    pub sequence: Arc<klipp_timeline::SharedSequence>,
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
    let (listener, path) = match claim_socket(&protocol::socket_path(), std::process::id()) {
        Ok(claimed) => claimed,
        Err(e) => {
            tracing::warn!("control socket unavailable: {e}");
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

/// Bind the control socket at `path`, or beside it when another Klipp
/// owns it.
///
/// A socket file left by a crashed instance would make bind fail
/// forever, so a file there used to be removed unconditionally. When a
/// Klipp was still running, that unlinked its live socket: the second
/// instance took every client over, and when it quit, left a dead file,
/// so the first was unreachable until it restarted (#400). Now the file
/// is connected to first. Refused, or gone: it is stale and replaced. A
/// live Klipp answers: its socket is left alone, and this instance binds
/// `<name>-<pid>.sock` in the same directory instead — reachable by
/// setting `KLIPP_CONTROL_SOCKET` to it.
fn claim_socket(path: &std::path::Path, pid: u32) -> Result<(UnixListener, std::path::PathBuf), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let path = if path.exists() && UnixStream::connect(path).is_ok() {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "klipp-control".into());
        let own = path.with_file_name(format!("{stem}-{pid}.sock"));
        tracing::warn!(
            "another Klipp is listening on {}; this one listens on {} (set KLIPP_CONTROL_SOCKET to reach it)",
            path.display(),
            own.display()
        );
        own
    } else {
        path.to_path_buf()
    };
    // Stale: nobody answered on it (or this pid's own leftover).
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path).map(|l| (l, path.clone())).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod socket_claim_tests {
    use super::claim_socket;
    use std::os::unix::net::UnixStream;

    fn dir() -> std::path::PathBuf {
        let d = std::path::Path::new("/tmp").join(format!("klipp-claim-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// #400: a second instance leaves a live Klipp's socket alone and
    /// binds its own beside it; both stay reachable.
    #[test]
    fn a_live_socket_is_not_taken_over() {
        let d = dir();
        let path = d.join("klipp-control.sock");
        let (_first, p1) = claim_socket(&path, 100).unwrap();
        assert_eq!(p1, path);
        let (_second, p2) = claim_socket(&path, 200).unwrap();
        assert_eq!(p2, d.join("klipp-control-200.sock"));
        assert!(UnixStream::connect(&path).is_ok(), "the first instance lost its socket");
        assert!(UnixStream::connect(&p2).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A socket file nobody listens on — a crashed instance's — is
    /// replaced, as before.
    #[test]
    fn a_stale_socket_is_replaced() {
        let d = dir();
        let path = d.join("klipp-control.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists() && UnixStream::connect(&path).is_err(), "a dead socket file");
        let (_l, p) = claim_socket(&path, 300).unwrap();
        assert_eq!(p, path);
        assert!(UnixStream::connect(&path).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
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
                let key = req.idempotency_key;
                let fingerprint = serde_json::to_value(&req.command).unwrap_or_default();
                let run = || match handle(&ctx, req.command, &mut None) {
                    Ok(v) => Response::ok(id, with_project_generation(&ctx, v)),
                    Err(e) => Response::err(id, e),
                };
                if let Some(key) = key {
                    if key.is_empty() || key.len() > 256 {
                        Response::err(id, "idempotency_key must be 1 to 256 bytes")
                    } else {
                        operations::with_idempotency_key(&key, fingerprint, id, run)
                    }
                } else {
                    run()
                }
            }
            // Parse failures answer on id 0 rather than dropping the
            // line: a client that sent malformed JSON should be told so,
            // not left waiting.
            Err(e) => {
                let id = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|v| v["id"].as_u64()).unwrap_or(0);
                Response::err(id, explain_bad_request(&line, &e))
            }
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

fn marker_color(s: &str) -> Result<klipp_core::MarkerColor, String> {
    use klipp_core::MarkerColor::*;
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "red" => Red,
        "orange" => Orange,
        "yellow" => Yellow,
        "green" => Green,
        "blue" => Blue,
        "purple" => Purple,
        other => return Err(format!("no marker colour \"{other}\": red, orange, yellow, green, blue or purple")),
    })
}

fn marker_kind(s: &str) -> Result<klipp_core::MarkerKind, String> {
    use klipp_core::MarkerKind::*;
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "" | "none" => None,
        "chapter" => Chapter,
        "todo" | "to_do" => ToDo,
        "comment" => Comment,
        "best_take" => BestTake,
        "section" => Section,
        other => return Err(format!("no marker kind \"{other}\": chapter, todo, comment, best_take or section")),
    })
}

fn marker_kind_name(k: klipp_core::MarkerKind) -> &'static str {
    use klipp_core::MarkerKind::*;
    match k {
        None => "none",
        Chapter => "chapter",
        ToDo => "todo",
        Comment => "comment",
        BestTake => "best_take",
        Section => "section",
    }
}

/// The marker named by `id`, when the timeline has it.
fn find_marker(ctx: &ControlContext, id: &str) -> Result<klipp_core::MarkerId, String> {
    let id = klipp_core::MarkerId(parse_uuid(id, "marker_id")?);
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    if seq.markers.iter().any(|m| m.id == id) {
        Ok(id)
    } else {
        Err(format!("no marker {} on the timeline (list_markers has them)", id.0))
    }
}

/// A frame rate as a fraction: the NTSC rates (23.976, 29.97, 47.95,
/// 59.94, 119.88) as n*1000/1001, the rest as whole numbers.
fn fps_fraction(fps: f64) -> Result<(u32, u32), String> {
    for base in [24u32, 30, 48, 60, 120] {
        if (fps - base as f64 * 1000.0 / 1001.0).abs() < 0.01 {
            return Ok((base * 1000, 1001));
        }
    }
    let whole = fps.round();
    if (fps - whole).abs() < 0.01 && (1.0..=240.0).contains(&whole) {
        return Ok((whole as u32, 1));
    }
    Err(format!("{fps} fps is not a frame rate Klipp takes: a whole number, or 23.976, 29.97, 59.94"))
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
        .map(|c| c.own_grade())
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

/// Where a transition sits on its track (#711): the cut between its two
/// clips, and the stretch it covers there by its alignment, in seconds.
/// `None` when its first clip is no longer on the track.
fn transition_span(t: &klipp_timeline::Track, tr: &klipp_timeline::Transition) -> Option<(f64, f64, f64)> {
    let cut = t.find_clip(tr.clip_a_id)?.timeline_out.to_secs_f64();
    let d = tr.duration.to_secs_f64();
    let (from, to) = match tr.alignment {
        klipp_core::TransitionAlignment::Center => (cut - d / 2.0, cut + d / 2.0),
        klipp_core::TransitionAlignment::StartAtCut => (cut, cut + d),
        klipp_core::TransitionAlignment::EndAtCut => (cut - d, cut),
    };
    Some((cut, from, to))
}

/// A transition's alignment in words, and as a client names it.
fn transition_alignment(a: klipp_core::TransitionAlignment) -> (&'static str, &'static str) {
    match a {
        klipp_core::TransitionAlignment::Center => ("centred on the cut", "center"),
        klipp_core::TransitionAlignment::StartAtCut => ("after the cut", "start_at_cut"),
        klipp_core::TransitionAlignment::EndAtCut => ("before the cut", "end_at_cut"),
    }
}

/// Part of the timeline to read: one track, and what plays in a stretch
/// (#796). The default is all of it.
#[derive(Debug, Clone, Copy, Default)]
struct TimelineView {
    track: Option<usize>,
    from_secs: Option<f64>,
    to_secs: Option<f64>,
}

impl TimelineView {
    fn shows(&self, a: f64, b: f64) -> bool {
        self.from_secs.is_none_or(|f| b > f) && self.to_secs.is_none_or(|t| a < t)
    }

    fn narrowed(&self) -> bool {
        self.track.is_some() || self.from_secs.is_some() || self.to_secs.is_some()
    }
}

/// The timeline as text: what a model reads before it plans. Tracks top
/// first; every clip with its range in seconds and frames at the
/// sequence's rate; the clip's kind and source; its grade and effects in
/// a word; the transitions on each track; the markers. Ids are there because an operation needs them,
/// at the end of the line where they cost the least attention.
fn timeline_text(
    seq: &klipp_timeline::Sequence,
    pool: &klipp_media::MediaPool,
    playhead_secs: Option<f64>,
    revision: u64,
    view: TimelineView,
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
    if view.narrowed() {
        let mut said = Vec::new();
        if let Some(t) = view.track {
            said.push(format!("track {t} only"));
        }
        match (view.from_secs, view.to_secs) {
            (Some(a), Some(b)) => said.push(format!("what plays from {a:.2} to {b:.2} s")),
            (Some(a), None) => said.push(format!("what plays from {a:.2} s on")),
            (None, Some(b)) => said.push(format!("what plays before {b:.2} s")),
            (None, None) => {}
        }
        let _ = writeln!(out, "Showing {}.", said.join(", "));
    }
    for (i, t) in seq.tracks.iter().enumerate() {
        if view.track.is_some_and(|w| w != i) {
            continue;
        }
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
        for c in t.clips.iter().filter(|c| view.shows(c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())) {
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
                klipp_timeline::ClipKind::Precomposition { .. } => "precomposition".to_string(),
                klipp_timeline::ClipKind::TemplatePrecomposition { .. } => "template precomposition".to_string(),
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
            let own = c.own_grade();
            let grade = if own.is_identity() { "none".to_string() } else { format!("{} nodes", own.nodes.len()) };
            let _ = write!(line, "  grade {grade}");
            // The grade group it grades inside (#237).
            if let Some(g) = c.grade_group.and_then(|g| seq.grade_group(g)) {
                let _ = write!(line, " in group \"{}\"", g.name);
            }
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
            if c.multicam_group.is_some() {
                // Counted from 1, as switch_angle takes them; the
                // primary (angle 1) carries the cuts, in timeline seconds.
                let _ = write!(line, "  multicam angle {}", c.multicam_angle as usize + 1);
                if !c.multicam_cuts.is_empty() {
                    let cuts: Vec<String> = c.multicam_cuts.iter().map(|(at, a)| format!("{:.2}s→{}", c.timeline_in.to_secs_f64() + at, *a as usize + 1)).collect();
                    let _ = write!(line, "  cuts {}", cuts.join(" "));
                }
            }
            let _ = write!(line, "  id {}", c.id.0);
            let _ = writeln!(out, "{line}");
        }
        // The transitions on the track's cuts (#711): what an agent that
        // made a dissolve reads back to see it there.
        for tr in &t.transitions {
            if transition_span(t, tr).is_some_and(|(_, from, to)| !view.shows(from, to)) {
                continue;
            }
            let spec = klipp_core::spec_for_kind(tr.kind);
            let d = tr.duration.to_secs_f64();
            let (words, _) = transition_alignment(tr.alignment);
            let mut line = format!("  transition {} ({}) {:.2} s ({} f)", spec.display_name, spec.string_key, d, (d * fps).round() as i64);
            match transition_span(t, tr) {
                Some((cut, from, to)) => {
                    let _ = write!(
                        line,
                        "  [{:.2}-{:.2} s | {}-{} f]  {words} at {:.2} s",
                        from, to, (from * fps).round() as i64, (to * fps).round() as i64, cut,
                    );
                }
                None => line.push_str("  (its first clip is gone)"),
            }
            let _ = write!(line, "  from {} to {}  id {}", tr.clip_a_id.0, tr.clip_b_id.0, tr.id.0);
            let _ = writeln!(out, "{line}");
        }
    }
    let marker_span = |m: &klipp_timeline::Marker| {
        let a = m.position.to_secs_f64();
        (a, m.duration.map_or(a, |d| (m.position + d).to_secs_f64()))
    };
    let markers: Vec<&klipp_timeline::Marker> = seq.markers.iter().filter(|m| {
        let (a, b) = marker_span(m);
        view.from_secs.is_none_or(|f| b >= f) && view.to_secs.is_none_or(|t| a < t)
    }).collect();
    if !markers.is_empty() {
        let _ = writeln!(out, "\nMarkers");
        for m in markers {
            let at = match m.duration {
                Some(d) => format!("{:.2}-{:.2} s", m.position.to_secs_f64(), (m.position + d).to_secs_f64()),
                None => format!("{:.2} s", m.position.to_secs_f64()),
            };
            let mut line = format!("  {at}  \"{}\"  {:?}", m.label, m.color);
            if !matches!(m.kind, klipp_core::MarkerKind::None) {
                let _ = write!(line, "  {}", marker_kind_name(m.kind));
            }
            if !m.comment.is_empty() {
                let _ = write!(line, "  note \"{}\"", m.comment.replace('\n', " "));
            }
            let _ = write!(line, "  id {}", m.id.0);
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// `text` within `max_chars`, cut on a line boundary and saying so, and
/// whether it was cut.
/// Where the user's viewer was before a frame read moved it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HeldTransport {
    at: Option<klipp_core::Timecode>,
    playing: bool,
}

/// Note where the user is before a read moves the playhead. The UI's
/// own playhead, as it publishes it, not the last frame the viewer
/// presented: that lags a seek, and in a window that is not being drawn
/// it can be minutes stale, so a read "put back" to it left the
/// playhead, and the Colour page following it, somewhere else.
fn hold_transport(ctx: &ControlContext) -> Result<HeldTransport, String> {
    let published = ctx.shared.transport.lock().map(|t| *t).unwrap_or_default();
    let presented = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.shown_presented();
    Ok(held_from(published, presented))
}

fn held_from(published: klipp_core::TransportStatus, presented: Option<klipp_core::Timecode>) -> HeldTransport {
    HeldTransport { at: published.playhead.or(presented), playing: published.playing }
}

/// What puts the viewer back after a read: the playhead where it was,
/// and playback running again if it was running.
fn restore_commands(held: HeldTransport) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    if let Some(t) = held.at {
        cmds.push(AppCommand::SeekTo(t));
    }
    if held.playing {
        cmds.push(AppCommand::Play);
    }
    cmds
}

fn restore_transport(ctx: &ControlContext, held: HeldTransport) {
    let cmds = restore_commands(held);
    if !cmds.is_empty() {
        let _ = send_envelope(ctx, cmds, false);
    }
}

#[cfg(test)]
mod made_media_tests {
    /// #730: downloaded music, effects and narration land in the kept
    /// made-media folder, never the cache, so a project's save can adopt
    /// them and a cache clean cannot take them.
    #[test]
    fn downloads_land_in_the_kept_made_media_folder() {
        assert_eq!(klipp_audio_search::download_dir(), klipp_media::made::made_dir("audio"));
        assert!(!klipp_audio_search::download_dir().starts_with(klipp_media::disk_cache::cache_base_dir()));
    }
}

#[cfg(test)]
mod next_step_tests {
    use super::*;

    fn placed() -> Vec<klipp_document::ops::Op> {
        let place: klipp_document::ops::Op = serde_json::from_value(json!({ "op": "place", "media_id": "00000000-0000-0000-0000-000000000000", "track": "video", "at": { "secs": 0 } })).unwrap();
        vec![place.clone(), place]
    }

    /// #743: the next step after the selects follows the brief: a piece
    /// that is not captioned is not told to caption or to cover joins.
    #[test]
    fn the_next_step_reads_the_brief() {
        let ops = placed();
        let trail = Trail { pictures_placed: true, ..Default::default() };
        let told = next_after_ops(&ops, None, trail).unwrap();
        assert!(told.contains("captions"), "{told}");
        let brief = klipp_timeline::Brief { captions: false, music: true, ..Default::default() };
        let told = next_after_ops(&ops, Some(&brief), trail).unwrap();
        assert!(!told.contains("captions") && !told.contains("jump cut") && told.contains("beat"), "{told}");
        // Without music, nothing about a beat (#758 review).
        let silent = klipp_timeline::Brief { captions: false, music: false, ..Default::default() };
        let told = next_after_ops(&ops, Some(&silent), trail).unwrap();
        assert!(!told.contains("captions") && !told.contains("beat"), "{told}");
    }

    /// #785: sound effects placed are not the selects, and what the
    /// timeline already has (titles, a grade) is not advised again.
    #[test]
    fn the_next_step_reads_the_timeline() {
        let ops = placed();
        let brief = klipp_timeline::Brief { captions: false, music: true, ..Default::default() };
        let sound = next_after_ops(&ops, Some(&brief), Trail::default()).unwrap();
        assert!(!sound.contains("selects") && sound.contains("Hear it"), "{sound}");
        let fresh = next_after_ops(&ops, Some(&brief), Trail { pictures_placed: true, ..Default::default() }).unwrap();
        assert!(fresh.ends_with("then titles in the brief's type styles, the grade, and check."), "{fresh}");
        let titled = next_after_ops(&ops, Some(&brief), Trail { pictures_placed: true, titled: true, graded: false }).unwrap();
        assert!(!titled.contains("titles") && titled.ends_with("then the grade and check."), "{titled}");
        let done = next_after_ops(&ops, Some(&brief), Trail { pictures_placed: true, titled: true, graded: true }).unwrap();
        assert!(done.ends_with("(klipp_beats), then check."), "{done}");
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use klipp_core::{Timecode, TransportStatus};

    #[test]
    fn the_published_playhead_wins_over_a_stale_presented_frame() {
        let held = held_from(
            TransportStatus { playhead: Some(Timecode::from_frames(150, 30)), playing: false },
            Some(Timecode::from_frames(5307, 30)),
        );
        assert_eq!(held.at, Some(Timecode::from_frames(150, 30)));
    }

    #[test]
    fn with_nothing_published_the_presented_frame_is_used() {
        let held = held_from(TransportStatus::default(), Some(Timecode::from_frames(30, 30)));
        assert_eq!(held.at, Some(Timecode::from_frames(30, 30)));
    }

    #[test]
    fn an_empty_buffer_is_not_a_picture_but_a_black_frame_is() {
        assert!(!has_picture(&[0u8; 16]));
        assert!(has_picture(&[0, 0, 0, 255, 0, 0, 0, 255]), "an opaque black frame is a real picture");
    }

    #[test]
    fn a_read_during_playback_seeks_back_and_plays_on() {
        let t = Timecode::from_frames(90, 30);
        let cmds = restore_commands(HeldTransport { at: Some(t), playing: true });
        assert!(matches!(cmds.as_slice(), [AppCommand::SeekTo(s), AppCommand::Play] if *s == t), "{cmds:?}");
    }
}

/// Seek to `secs`, wait for the frame there, read its RGBA bytes, and
/// put the user's playhead back — the picture as the viewer shows it,
/// for measuring. Shares the one-grab lock and the wait with frame_at.
/// How long a frame read waits by default. A window that is on screen
/// renders a 4K frame in about a tenth of a second; one on a workspace
/// that is not being shown renders the same frame in one to three
/// seconds, and the old three-second default failed most reads there.
const FRAME_READ_TIMEOUT_MS: u64 = 10_000;

/// Whether a frame buffer holds a picture at all. A rendered frame is
/// opaque; a buffer that is all zero, alpha included, is one nothing
/// has been written to yet, and measuring it reported a black frame
/// that was never on the timeline.
fn has_picture(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|p| p[3] != 0)
}

fn grab_frame_rgba(ctx: &ControlContext, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    grab_frame_rgba_of(ctx, secs, None, timeout_ms)
}

/// [`grab_frame_rgba`], with `only` the frame is that clip alone —
/// graded and placed, with nothing over or under it (#719): what a colour
/// tool measures a clip by, so a title, a matte or a colour card over it
/// is not taken for its footage. The viewer is back to the whole picture
/// before this returns, whatever happened.
fn grab_frame_rgba_of(ctx: &ControlContext, secs: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
    let held = hold_transport(ctx)?;
    let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
    let target = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
    let before = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_generation();
    let mut seek = vec![AppCommand::Pause];
    if only.is_some() {
        seek.push(AppCommand::SetMeasureOnlyClip(only));
    }
    seek.push(AppCommand::SeekTo(target));
    send_envelope(ctx, seek, false)?;
    // Whole again before the transport is put back, so the frame the
    // viewer is left on is the composite — on every way out, an early
    // return among them, as the guard clears it when dropped.
    let mut solo = SoloRead { ctx, active: only.is_some(), held: Some(held), read_at: target };
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(FRAME_READ_TIMEOUT_MS));
    let deadline = std::time::Instant::now() + wait;
    loop {
        let (g, at, complete) = {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            (fb.latest_generation(), fb.latest_presented(), fb.latest_complete())
        };
        if g != before && at.is_some_and(|a| a.cmp(&target) == std::cmp::Ordering::Equal) && complete {
            break;
        }
        if std::time::Instant::now() >= deadline {
            solo.end();
            return Err(format!("no frame at {secs:.3} s within {} ms; give a longer timeout_ms", wait.as_millis()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The bytes: the stack presents zero-copy and the frame buffer's copy
    // can be a beat behind the texture, so a short wait for them.
    let (data, width, height) = loop {
        let (data, width, height) = {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            let (d, w, h, _) = fb.latest_frame();
            (d.to_vec(), w, h)
        };
        let expected = (width as usize) * (height as usize) * 4;
        if width > 0 && height > 0 && data.len() >= expected && has_picture(&data[..expected]) {
            break (data, width, height);
        }
        if std::time::Instant::now() >= deadline {
            solo.end();
            return Err(format!(
                "the viewer has no picture of {secs:.3} s to measure yet (its copy of the frame is empty); \
                 a window that is not on screen renders slowly, so give a longer timeout_ms"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    solo.end();
    let expected = (width as usize) * (height as usize) * 4;
    Ok((data[..expected].to_vec(), width, height))
}

/// A frame read in progress: ending it, or dropping it on an early
/// return, puts the viewer back as it was — the engine rendering the
/// whole picture again after a clip-alone read (#719), then the playhead
/// where it was and playback running if it ran; with no playhead to go
/// back to, the frame it read redrawn whole (#749, #753 reviews).
struct SoloRead<'a> {
    ctx: &'a ControlContext,
    active: bool,
    held: Option<HeldTransport>,
    read_at: klipp_core::Timecode,
}

impl SoloRead<'_> {
    fn end(&mut self) {
        if std::mem::take(&mut self.active) {
            let _ = send_envelope(self.ctx, vec![AppCommand::SetMeasureOnlyClip(None)], false);
        }
        if let Some(held) = self.held.take() {
            let repaint = held.at.is_none();
            restore_transport(self.ctx, held);
            if repaint {
                let _ = send_envelope(self.ctx, vec![AppCommand::SeekTo(self.read_at)], false);
            }
        }
    }
}

impl Drop for SoloRead<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

/// A GPU-drawn scope image (`mode`: 0 waveform, 1 parade, 2 vectorscope,
/// 3 histogram) of the frame at `secs`, as RGBA plus its size. Grabs the
/// frame, has the engine draw the scope into its snapshot store — apart
/// from the viewer's live scopes — and reads it back.
fn grab_scope(ctx: &ControlContext, mode: u8, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    let (rgba, w, h) = grab_frame_rgba(ctx, secs, timeout_ms)?;
    let before = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?
        .snapshot_scope_textures.get(&mode).map(|e| e.generation);
    send_envelope(ctx, vec![AppCommand::RenderScopeSnapshot { modes: vec![mode], rgba, width: w, height: h }], false)?;
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(FRAME_READ_TIMEOUT_MS));
    let deadline = std::time::Instant::now() + wait;
    loop {
        {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            if let Some(e) = fb.snapshot_scope_textures.get(&mode) {
                if !e.data.is_empty() && Some(e.generation) != before {
                    return Ok((e.data.clone(), e.w, e.h));
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("the scope was not drawn within {} ms; give a longer timeout_ms, or the frame had nothing to measure", wait.as_millis()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The scope readings of a display-referred sRGB RGBA frame: what a
/// colourist reads before touching a wheel. Luma is Rec.709 on
/// gamma-encoded values, in IRE (0 black, 100 white), which is how
/// waveforms are read. Everything is measured on a grid of samples, not
/// every pixel — enough to be exact to a fraction and cheap on a 4K
/// frame.
/// The colour faults in a frame's [`frame_stats`] reading, each as
/// (secs, what, hint): what a colourist would flag before delivery.
pub fn color_faults(stats: &serde_json::Value, secs: f64) -> Vec<(f64, String, String)> {
    let mut out = Vec::new();
    let f = |v: &serde_json::Value, a: &str, b: &str| v.get(a).and_then(|x| x.get(b)).and_then(|x| x.as_f64());
    let g = |v: &serde_json::Value, a: &str| v.get(a).and_then(|x| x.as_f64());
    if let Some(hi) = f(stats, "clipping_pct", "high") {
        if hi > 2.0 {
            out.push((secs, format!("{hi:.0}% of the frame is clipped white"), "Bring the highlights down (grade highlights or gain negative) so detail comes back".into()));
        }
    }
    if let Some(lo) = f(stats, "clipping_pct", "low") {
        if lo > 2.0 {
            out.push((secs, format!("{lo:.0}% of the frame is crushed to black"), "Lift the shadows (grade shadows or lift positive) so detail comes back".into()));
        }
    }
    if let Some(white) = f(stats, "luma_ire", "white") {
        if white < 70.0 {
            out.push((secs, format!("the brightest part reaches only {white:.0} IRE — the picture is dull"), "Raise the white point (grade gain or contrast) toward 100".into()));
        }
    }
    if let Some(sat) = g(stats, "saturation_pct") {
        if sat > 60.0 {
            out.push((secs, format!("saturation is high at {sat:.0}% — colours may be over-cooked"), "Bring saturation down, or use vibrance which protects skin".into()));
        }
    }
    if let Some(note) = stats.get("skin").and_then(|s| s.as_str()) {
        if note.contains("sit on") {
            if let Some(pct) = note.split('%').next().and_then(|n| n.trim().parse::<f64>().ok()) {
                if pct < 60.0 {
                    out.push((secs, format!("only {pct:.0}% of skin tones sit on the skin-tone line"), "Nudge the white balance (temperature, tint) until faces land on the line".into()));
                }
            }
        }
    }
    out
}

/// Where the bundled LUT library sits: next to the executable, or in the
/// source tree at build time. The one lookup the app and projects share.
fn bundled_luts_dir() -> Option<std::path::PathBuf> {
    klipp_core::assets::bundled_luts_dir().map(|p| p.to_path_buf())
}

const LOOK_CATEGORIES: &[(&str, &str)] = &[
    ("cinematic", "Cinematic"),
    ("film-bw", "Film B&W"),
    ("film-slide", "Film Slide"),
    ("film-negative", "Film Negative"),
    ("film-print", "Film Print"),
    ("film-instant", "Film Instant"),
    ("fuji-xtrans", "Fuji X-Trans"),
];

/// One bundled look: its category, its display name (the file stem), path.
fn scan_looks() -> Vec<(String, String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Some(base) = bundled_luts_dir() else { return out };
    for (dir, _label) in LOOK_CATEGORIES {
        let cat = base.join(dir);
        let Ok(rd) = std::fs::read_dir(&cat) else { continue };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "cube" || x == "3dl" || x == "csp"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let stem = e.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            out.push((dir.to_string(), stem, e.path()));
        }
    }
    out
}

/// Resolve a look name (or an absolute .cube path) to a file and a
/// display name. A query matches when every whitespace token appears in
/// the category or the file name; an exact name wins a tie.
pub fn resolve_look(name: &str) -> Result<(std::path::PathBuf, String), String> {
    let q = name.trim();
    // An explicit path to a real LUT file is taken as-is.
    let as_path = std::path::Path::new(q);
    if as_path.is_absolute() && as_path.is_file() {
        let stem = as_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| q.to_string());
        return Ok((as_path.to_path_buf(), stem));
    }
    let looks = scan_looks();
    if looks.is_empty() {
        return Err("the bundled LUT library was not found next to the app; give an absolute .cube path instead".into());
    }
    pick_look(&looks, q)
}

/// The look in `looks` (category, name, file) that `q` names: words that
/// all appear in "category name", where a `/` is a word break too, so the
/// `category/name` that `looks` lists is taken as it is shown (#656).
/// Several matching: the one whose name, or category/name, is `q`
/// exactly; else a list to choose from.
fn pick_look(looks: &[(String, String, std::path::PathBuf)], q: &str) -> Result<(std::path::PathBuf, String), String> {
    let ql = q.trim().to_lowercase();
    let tokens: Vec<&str> = ql.split(|c: char| c.is_whitespace() || c == '/').filter(|t| !t.is_empty()).collect();
    let hay = |cat: &str, stem: &str| format!("{cat} {stem}").to_lowercase();
    let matches: Vec<&(String, String, std::path::PathBuf)> = looks.iter()
        .filter(|(cat, stem, _)| tokens.iter().all(|t| hay(cat, stem).contains(t)))
        .collect();
    match matches.len() {
        0 => {
            let sample: Vec<String> = looks.iter().take(8).map(|(c, s, _)| format!("{c}/{s}")).collect();
            Err(format!("no look matches \"{q}\"; call looks to see the library. A few: {}", sample.join(", ")))
        }
        1 => Ok((matches[0].2.clone(), matches[0].1.clone())),
        _ => {
            // An exact name, or category/name, settles it; otherwise ask
            // to be specific.
            if let Some(exact) = matches.iter().find(|(c, s, _)| s.to_lowercase() == ql || format!("{c}/{s}").to_lowercase() == ql) {
                return Ok((exact.2.clone(), exact.1.clone()));
            }
            let names: Vec<String> = matches.iter().take(10).map(|(c, s, _)| format!("{c}/{s}")).collect();
            Err(format!("\"{q}\" matches {} looks; be more specific: {}", matches.len(), names.join(", ")))
        }
    }
}

#[cfg(test)]
mod timeline_view_tests {
    use super::*;

    /// #796: one track and one stretch of the timeline can be read alone,
    /// so a long cut stays readable inside the reply's budget.
    #[test]
    fn the_timeline_reads_a_track_and_a_stretch() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let scene = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = vec![
            klipp_timeline::Clip::sequence(scene.id, tc(0.0), tc(2.0)),
            klipp_timeline::Clip::sequence(scene.id, tc(2.0), tc(4.0)),
        ];
        let (first, second) = (seq.tracks[0].clips[0].id.0.to_string(), seq.tracks[0].clips[1].id.0.to_string());
        let pool = klipp_media::MediaPool::new();
        let all = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(all.contains(&first) && all.contains(&second) && all.contains("[1] Audio"));
        let late = timeline_text(&seq, &pool, None, 1, TimelineView { track: Some(0), from_secs: Some(2.5), to_secs: None });
        assert!(!late.contains(&first) && late.contains(&second), "{late}");
        assert!(!late.contains("[1] Audio") && late.contains("Showing track 0 only, what plays from 2.50 s on."), "{late}");
    }
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    /// #795: listen names results by count or by id, five at most.
    #[test]
    fn listen_picks_by_count_or_id() {
        let req: ControlCommand = serde_json::from_value(json!({ "method": "search_sfx", "query": "whoosh", "listen": ["42", "7"], "max_secs": 3.0 })).unwrap();
        let ControlCommand::SearchSfx { listen: Some(listen), max_secs: Some(m), .. } = req else { panic!("{req:?}") };
        assert_eq!(m, 3.0);
        assert!(listen.picks(9, "7") && listen.picks(0, "42") && !listen.picks(0, "8"));
        let req: ControlCommand = serde_json::from_value(json!({ "method": "search_music", "query": "folk", "listen": 9 })).unwrap();
        let ControlCommand::SearchMusic { listen: Some(listen), .. } = req else { panic!() };
        assert!(listen.picks(4, "x") && !listen.picks(5, "x"), "at most five");
        assert!(Listen::First(0).is_none() && Listen::Ids(Vec::new()).is_none());
    }

    /// A short sound's length, when it peaks and how loud: silence, then
    /// a burst at 0.40 s.
    #[test]
    fn a_sound_effect_is_heard_for_where_it_peaks() {
        let sr = 22_050u32;
        let mut pcm = vec![0.001f32; sr as usize];
        for (i, x) in pcm.iter_mut().enumerate().skip((0.40 * sr as f32) as usize).take((0.05 * sr as f32) as usize) {
            *x = 0.8 * ((i as f32) * 0.3).sin();
        }
        let l = SfxListen::from_pcm(&pcm, sr);
        assert!((l.secs - 1.0).abs() < 1e-3, "{l:?}");
        assert!((l.peak_secs - 0.45).abs() < 0.06, "{l:?}");
        assert!(l.peak_db > -3.0 && l.peak_db < 0.0, "{l:?}");
        assert!(l.describe().contains("loudest at"));
    }

    /// No more than five previews are fetched, whatever is asked.
    #[test]
    fn at_most_five_previews_are_heard() {
        let urls: Vec<Option<String>> = (0..8).map(|i| Some(format!("https://example.invalid/{i}"))).collect();
        let heard = hear_previews(urls, |_| Ok::<u8, String>(1));
        assert_eq!(heard.iter().filter(|h| h.is_some()).count(), 5);
        assert!(heard[5..].iter().all(|h| h.is_none()));
    }
}

#[cfg(test)]
mod look_name_tests {
    use super::pick_look;

    /// #656: a look named as `looks` lists it, category/name, is found;
    /// words still work; an exact category/name settles a tie.
    #[test]
    fn a_look_is_found_by_its_listed_name() {
        let look = |c: &str, s: &str| (c.to_string(), s.to_string(), std::path::PathBuf::from(format!("/luts/{c}/{s}.cube")));
        let looks = vec![look("film-print", "Kodak 2383"), look("film-print", "Kodak 2383 D60"), look("cinematic", "Teal Orange")];
        assert_eq!(pick_look(&looks, "cinematic/Teal Orange").unwrap().1, "Teal Orange");
        assert_eq!(pick_look(&looks, "teal orange").unwrap().1, "Teal Orange");
        assert_eq!(pick_look(&looks, "film-print/Kodak 2383").unwrap().1, "Kodak 2383", "exact settles the tie with D60");
        assert!(pick_look(&looks, "film-print/kodak").unwrap_err().contains("matches 2 looks"));
        assert!(pick_look(&looks, "nothing/here").is_err());
    }
}

/// When nothing at `tc` can be drawn because every picture there is a
/// file that is missing: the first of them, said as "<clip> is offline:
/// <path> is not there" (#657). One missing layer under another that is
/// there still gives a frame — the engine draws what opens — and is not
/// a reason to refuse one.
fn offline_picture_at(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, tc: klipp_core::Timecode) -> Option<String> {
    let clips: Vec<&klipp_timeline::Clip> = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .filter_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))
        .filter(|c| c.enabled && !matches!(c.kind, klipp_timeline::ClipKind::Gap))
        .collect();
    // A clip's file that is missing, with no ready proxy to stand in.
    let missing = |c: &klipp_timeline::Clip| -> Option<String> {
        let item = pool.get(&c.media_id()?)?;
        let proxy = item.proxy_ready && item.proxy_path.as_ref().is_some_and(|p| p.exists());
        ((item.offline || !item.path.exists()) && !proxy).then(|| format!("{} is offline: {} is not there", item.filename, item.path.display()))
    };
    // Anything there that draws — a file that is there, a title, a
    // generator — and the frame is not refused.
    if clips.is_empty() || clips.iter().any(|c| missing(c).is_none()) {
        return None;
    }
    clips.iter().find_map(|c| missing(c))
}

#[cfg(test)]
mod offline_frame_tests {
    use super::offline_picture_at;

    /// #657: a picture clip whose file is missing is named at once; one
    /// whose file is there is not.
    #[test]
    fn a_missing_file_under_the_time_is_named() {
        let mut pool = klipp_media::MediaPool::new();
        let gone = pool.add(klipp_media::MediaItem::synthetic_for_test("/nowhere/gone.mov".into(), None));
        let here_path = std::env::temp_dir().join(format!("klipp-here-{}.mov", std::process::id()));
        std::fs::write(&here_path, b"x").unwrap();
        let here = pool.add(klipp_media::MediaItem::synthetic_for_test(here_path.clone(), None));
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v.clips = vec![
            klipp_timeline::Clip::media(gone, tc(0.0), tc(2.0), tc(0.0), tc(2.0)),
            klipp_timeline::Clip::media(here, tc(2.0), tc(4.0), tc(0.0), tc(2.0)),
        ];
        seq.tracks = vec![v];
        let said = offline_picture_at(&seq, &pool, tc(1.0)).expect("the missing file is named");
        assert!(said.contains("/nowhere/gone.mov") && said.contains("offline"), "{said}");
        assert_eq!(offline_picture_at(&seq, &pool, tc(3.0)), None);
        // An online clip on a track above the gone one draws: the frame is
        // not refused for the layer under it (#660 review).
        let mut top = klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video);
        top.clips = vec![klipp_timeline::Clip::media(here, tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        seq.tracks.insert(0, top);
        assert_eq!(offline_picture_at(&seq, &pool, tc(1.0)), None);
        std::fs::remove_file(&here_path).ok();
    }
}

/// The grade knobs that move a shot toward a reference shot, from the
/// two frames' scope readings: lift from the black-point gap, gain from
/// the white-point gap, exposure from the mid gap, and temperature and
/// tint from the RGB balance. A first pass a colourist would refine, not
/// a final answer — verify with the colour check and nudge. A knob whose
/// gap is within tolerance is left alone (`None`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MatchKnobs {
    pub exposure: Option<f32>,
    pub lift: Option<f32>,
    pub gain: Option<f32>,
    pub temperature: Option<f32>,
    pub tint: Option<f32>,
}

impl MatchKnobs {
    /// Nothing to do: every knob is within tolerance.
    pub fn is_empty(&self) -> bool {
        self.exposure.is_none() && self.lift.is_none() && self.gain.is_none() && self.temperature.is_none() && self.tint.is_none()
    }
}

pub fn match_grade_knobs(target: &serde_json::Value, reference: &serde_json::Value) -> MatchKnobs {
    let ire = |v: &serde_json::Value, k: &str| v.get("luma_ire").and_then(|x| x.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let bal = |v: &serde_json::Value, k: &str| v.get("rgb_balance").and_then(|x| x.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let some_if = |v: f32, thresh: f32| if v.abs() > thresh { Some(v) } else { None };

    // Black and white points: the gap in IRE, as a lift/gain offset.
    let lift = some_if(((ire(reference, "black") - ire(target, "black")) / 100.0).clamp(-0.3, 0.3) as f32, 0.015);
    let gain = some_if(((ire(reference, "white") - ire(target, "white")) / 100.0).clamp(-0.3, 0.3) as f32, 0.015);
    // Mid: the exposure in stops that brings the mid grey together.
    let (tm, rm) = (ire(target, "mid").max(0.5), ire(reference, "mid").max(0.5));
    let exposure = some_if((rm / tm).log2().clamp(-1.0, 1.0) as f32, 0.05);
    // Balance: warmth is red minus blue, tint is green against the rest.
    let warm = |v: &serde_json::Value| bal(v, "red") - bal(v, "blue");
    let green = |v: &serde_json::Value| bal(v, "green") - (bal(v, "red") + bal(v, "blue")) / 2.0;
    let temperature = some_if(((warm(reference) - warm(target)) * 4.0).clamp(-0.6, 0.6) as f32, 0.02);
    let tint = some_if(((green(reference) - green(target)) * 4.0).clamp(-0.6, 0.6) as f32, 0.02);

    MatchKnobs { exposure, lift, gain, temperature, tint }
}

/// A keep-range cut out of a long take: where it starts and ends in the
/// file, and what is said in it.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Select {
    pub in_secs: f64,
    pub out_secs: f64,
    pub text: String,
    /// True when the range is only filler (um, uh) or too short to use —
    /// a candidate to drop, not to keep.
    pub filler: bool,
}

fn is_filler_word(w: &str) -> bool {
    let t: String = w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    matches!(t.as_str(), "um" | "uh" | "uhh" | "umm" | "erm" | "er" | "hmm" | "mm" | "mmm" | "ah" | "eh")
}

/// Group a long take's words into selects — the runs of speech between
/// the real pauses — so an agent can keep the good takes and drop the
/// dead air and the filler, building a tight cut from a loose one. A gap
/// of `min_gap` seconds or more starts a new select; a run that is only
/// filler, or shorter than `min_len`, is flagged to drop.
pub fn suggest_selects(words: &[(f64, f64, String)], min_gap: f64, min_len: f64) -> Vec<Select> {
    let mut selects: Vec<Select> = Vec::new();
    let mut cur: Vec<&(f64, f64, String)> = Vec::new();
    let flush = |cur: &mut Vec<&(f64, f64, String)>, out: &mut Vec<Select>| {
        if cur.is_empty() {
            return;
        }
        let in_secs = cur.first().unwrap().0;
        let out_secs = cur.last().unwrap().1;
        let text = cur.iter().map(|w| w.2.trim()).collect::<Vec<_>>().join(" ");
        let all_filler = cur.iter().all(|w| is_filler_word(&w.2));
        let too_short = out_secs - in_secs < min_len;
        out.push(Select { in_secs, out_secs, text, filler: all_filler || too_short });
        cur.clear();
    };
    for w in words {
        if let Some(prev) = cur.last() {
            if w.0 - prev.1 >= min_gap {
                flush(&mut cur, &mut selects);
            }
        }
        cur.push(w);
    }
    flush(&mut cur, &mut selects);
    selects
}

/// Decode one frame of a media file at `secs` to 320x180 RGBA through
/// ffmpeg, for measuring a reference clip's look off the file (not the
/// timeline). Scaled small: the scope stats read colour and luma, which a
/// downscale preserves.
fn decode_frame_rgba(path: &std::path::Path, secs: f64) -> Result<(Vec<u8>, u32, u32), String> {
    let (w, h) = (320u32, 180u32);
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", &format!("{secs}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let need = (w * h * 4) as usize;
    if out.stdout.len() < need {
        return Err(format!("no frame decoded at {secs:.2} s"));
    }
    Ok((out.stdout[..need].to_vec(), w, h))
}

/// What one listen to a track's preview found.
struct PreviewListen {
    bpm: f32,
    beat_confidence: f32,
    loudness_db: f32,
    /// The level over eight equal parts of the track, dB.
    energy_db: Vec<f32>,
    shape: &'static str,
}

impl PreviewListen {
    fn describe(&self) -> String {
        let beat = if self.beat_confidence < 0.3 { "no clear beat".to_string() } else { format!("{:.0} BPM", self.bpm) };
        let curve: Vec<String> = self.energy_db.iter().map(|d| format!("{d:.0}")).collect();
        format!("{beat}, {:.0} dB, {} (dB by eighths: {})", self.loudness_db, self.shape, curve.join(" "))
    }
}

/// Hear a preview URL straight off the library through ffmpeg: nothing
/// is imported or kept.
fn listen_to_preview(url: &str) -> Result<PreviewListen, String> {
    const SR: u32 = 22_050;
    // Only a library's own https stream goes to ffmpeg, never a file, a
    // pipe or another protocol a result could name (#767 review).
    if !url.starts_with("https://") {
        return Err("the preview is not an https address".into());
    }
    let pcm = klipp_analysis::audio_sync::decode_mono_pcm(std::path::Path::new("ffmpeg"), std::path::Path::new(url), SR, 0.0, 420.0)
        .map_err(|_| "the preview could not be read".to_string())?;
    if pcm.len() < SR as usize * 2 {
        return Err("the preview is too short to judge".into());
    }
    let beats = klipp_analysis::beats::detect_from_pcm(&pcm, SR);
    let (energy_db, shape) = energy_shape(&pcm);
    Ok(PreviewListen { bpm: beats.bpm, beat_confidence: beats.confidence, loudness_db: rms_db(&pcm), energy_db, shape })
}

/// Listen to the previews given, each on a thread of its own, all of
/// them together within thirty seconds: a stalled stream is reported as
/// not heard rather than holding the reply (#767 review), and a listen
/// still running is left to end on ffmpeg's read timeout. `None` where
/// no preview was asked for.
fn hear_previews<T: Send + 'static>(urls: Vec<Option<String>>, hear: fn(&str) -> Result<T, String>) -> Vec<Option<Result<T, String>>> {
    let mut asked = 0;
    let receivers: Vec<_> = urls
        .into_iter()
        .map(|url| {
            // At most five heard, in the order the results came.
            let url = url.filter(|u| !u.is_empty() && asked < 5)?;
            asked += 1;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(hear(&url));
            });
            Some(rx)
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    receivers
        .into_iter()
        .map(|r| {
            r.map(|r| match r.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(heard) => heard,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("the preview did not answer within thirty seconds".into()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the listen failed".into()),
            })
        })
        .collect()
}

/// A sound effect's preview heard: how long it is, when it is loudest,
/// and how loud (#795).
#[derive(Debug, Clone, PartialEq)]
struct SfxListen {
    secs: f32,
    /// When the loudest tenth of a second is, from the start.
    peak_secs: f32,
    loudness_db: f32,
    peak_db: f32,
}

impl SfxListen {
    fn from_pcm(pcm: &[f32], sr: u32) -> SfxListen {
        let window = (sr as usize / 10).max(1);
        let (peak_at, _) = pcm.chunks(window).enumerate().map(|(i, w)| (i, rms_db(w))).fold((0, f32::MIN), |b, c| if c.1 > b.1 { c } else { b });
        let peak = pcm.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        SfxListen {
            secs: pcm.len() as f32 / sr as f32,
            peak_secs: (peak_at * window + window / 2).min(pcm.len()) as f32 / sr as f32,
            loudness_db: rms_db(pcm),
            peak_db: 20.0 * peak.max(1e-6).log10(),
        }
    }

    fn describe(&self) -> String {
        format!("{:.2} s, loudest at {:.2} s, {:.0} dB (peak {:.0} dB)", self.secs, self.peak_secs, self.loudness_db, self.peak_db)
    }
}

fn listen_to_sfx(url: &str) -> Result<SfxListen, String> {
    const SR: u32 = 22_050;
    // Only a library's own https stream goes to ffmpeg (#767 review).
    if !url.starts_with("https://") {
        return Err("the preview is not an https address".into());
    }
    let pcm = klipp_analysis::audio_sync::decode_mono_pcm(std::path::Path::new("ffmpeg"), std::path::Path::new(url), SR, 0.0, 120.0)
        .map_err(|_| "the preview could not be read".to_string())?;
    if pcm.is_empty() {
        return Err("the preview has no sound".into());
    }
    Ok(SfxListen::from_pcm(&pcm, SR))
}

fn rms_db(pcm: &[f32]) -> f32 {
    let ms = pcm.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / pcm.len().max(1) as f64;
    (10.0 * ms.max(1e-12).log10()) as f32
}

/// The level over eight equal parts of a track and a word for its course:
/// whether it builds, falls away, swells in the middle or holds steady.
fn energy_shape(pcm: &[f32]) -> (Vec<f32>, &'static str) {
    let n = 8;
    let part = (pcm.len() / n).max(1);
    let db: Vec<f32> = pcm.chunks(part).take(n).map(rms_db).collect();
    if db.len() < n {
        return (db, "steady");
    }
    let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    let (start, middle, end) = (mean(&db[..2]), mean(&db[3..5]), mean(&db[6..]));
    let spread = db.iter().cloned().fold(f32::MIN, f32::max) - db.iter().cloned().fold(f32::MAX, f32::min);
    let shape = if spread < 4.0 {
        "steady"
    } else if middle > start + 3.0 && middle > end + 3.0 {
        "swells in the middle"
    } else if end > start + 3.0 {
        "builds"
    } else if start > end + 3.0 {
        "falls away"
    } else {
        "varies"
    };
    (db, shape)
}

#[cfg(test)]
mod preview_tests {
    use super::{energy_shape, listen_to_preview};

    /// #767 review: only an https stream is handed to ffmpeg.
    #[test]
    fn only_an_https_preview_is_listened_to() {
        for url in ["file:///etc/passwd", "/etc/passwd", "http://example.com/a.mp3", "pipe:0"] {
            assert!(listen_to_preview(url).is_err_and(|e| e.contains("https")), "{url}");
        }
    }

    fn tone(levels: &[f32]) -> Vec<f32> {
        levels.iter().flat_map(|a| (0..1000).map(move |i| a * ((i as f32) * 0.3).sin())).collect()
    }

    /// #739: a track's course read from its level over eighths.
    #[test]
    fn energy_shapes_are_named() {
        assert_eq!(energy_shape(&tone(&[0.5; 8])).1, "steady");
        assert_eq!(energy_shape(&tone(&[0.05, 0.05, 0.1, 0.2, 0.3, 0.4, 0.6, 0.6])).1, "builds");
        assert_eq!(energy_shape(&tone(&[0.6, 0.6, 0.4, 0.3, 0.2, 0.1, 0.05, 0.05])).1, "falls away");
        assert_eq!(energy_shape(&tone(&[0.05, 0.05, 0.2, 0.6, 0.6, 0.2, 0.05, 0.05])).1, "swells in the middle");
    }
}

/// Summarise a reference's editing from its shots and beats: the cut
/// cadence, and how tightly the cuts fall on the beat. Pure, so it is
/// unit-tested without ffmpeg. `shots` are `(start, end)` in seconds,
/// `boundaries` the cut times, `beats` the tempo grid.
pub fn style_cut_summary(shots: &[(f64, f64)], boundaries: &[f64], duration: f64, beats: &klipp_analysis::beats::BeatData) -> serde_json::Value {
    let mut lengths: Vec<f64> = shots.iter().map(|(a, b)| b - a).filter(|d| *d > 0.0).collect();
    lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = lengths.len();
    let mean = if n > 0 { lengths.iter().sum::<f64>() / n as f64 } else { 0.0 };
    let median = if n > 0 { lengths[n / 2] } else { 0.0 };
    let cuts_per_min = if duration > 0.0 { boundaries.len() as f64 / (duration / 60.0) } else { 0.0 };
    let pace = if mean == 0.0 { "unknown" } else if mean < 1.5 { "fast" } else if mean < 4.0 { "medium" } else { "slow" };
    // How many cuts land on a beat (within a 12th of a second).
    let on_beat = if beats.beat_times.len() >= 2 && beats.confidence > 0.3 && !boundaries.is_empty() {
        let hits = boundaries.iter().filter(|&&c| {
            beats.beat_times.iter().any(|&b| (b - c).abs() < 0.12)
        }).count();
        Some(hits as f64 / boundaries.len() as f64)
    } else {
        None
    };
    serde_json::json!({
        "shot_count": n,
        "mean_shot_secs": (mean * 100.0).round() / 100.0,
        "median_shot_secs": (median * 100.0).round() / 100.0,
        "shortest_shot_secs": lengths.first().copied().unwrap_or(0.0),
        "longest_shot_secs": lengths.last().copied().unwrap_or(0.0),
        "cuts_per_min": (cuts_per_min * 10.0).round() / 10.0,
        "pace": pace,
        "cut_on_beat_ratio": on_beat,
    })
}

/// Evidence about development inside detected shots, separate from cut cadence.
/// Luma differences and freeze detection measure change, not the meaning of an
/// animation, and must not be converted into a universal shot-length recipe.
#[derive(serde::Serialize)]
struct PacingEvidence {
    within_shot_mean_ydif: Option<f32>,
    within_shot_sample_seconds: usize,
    valid_sample_seconds: usize,
    cut_exclusion: &'static str,
    within_shot_motion_class: Option<&'static str>,
    motion_evidence: &'static str,
    detected_holds: Option<Vec<serde_json::Value>>,
    hold_evidence: &'static str,
    guidance: String,
}

pub fn style_pacing_evidence(
    shots: &klipp_analysis::shots::Shots,
    motion: Option<&klipp_analysis::motion::Motion>,
    sheet: Option<&klipp_analysis::beatsheet::BeatSheet>,
) -> serde_json::Value {
    serde_json::to_value(pacing_evidence(shots, motion, sheet)).expect("finite pacing evidence")
}

fn pacing_evidence(
    shots: &klipp_analysis::shots::Shots,
    motion: Option<&klipp_analysis::motion::Motion>,
    sheet: Option<&klipp_analysis::beatsheet::BeatSheet>,
) -> PacingEvidence {
    // Preserve actual detector provenance. An exact beat-aligned fast cut
    // excludes only its own bin. Widen only known slow-only detections.
    let mut excluded = std::collections::HashSet::new();
    for &b in &shots.boundaries { excluded.insert(b.floor() as usize); }
    for &b in &shots.slow_boundaries { excluded.insert((b - 0.5).max(0.0).floor() as usize); }
    let mut valid = 0;
    let mut samples = Vec::new();
    if let Some(m) = motion {
        for (i, &v) in m.per_second.iter().enumerate() {
            if let Some(v) = v {
                valid += 1;
                if !excluded.contains(&i) { samples.push(v); }
            }
        }
    }
    let within_mean = klipp_analysis::motion::mean_of(samples.iter().copied());
    let holds = sheet.map(|s| s.segments.iter()
        .filter(|s| s.kind == "idle")
        .map(|s| serde_json::json!({"start_secs": s.start_secs, "end_secs": s.end_secs}))
        .collect::<Vec<_>>());
    let motion_class = within_mean.map(klipp_analysis::motion::class_for);
    let motion_evidence = match (motion.is_some(), valid, samples.len()) {
        (false, _, _) => "unavailable",
        (true, 0, _) => "no_valid_samples",
        (true, _, 0) => "excluded_by_cuts",
        _ => "measured",
    };
    let mut guidance = String::from("Detected-shot durations describe cut cadence only; they are not target hold lengths or animation timings. ");
    match motion_class {
        Some("static") => guidance.push_str("Measured within-shot change is low, but small animated elements can be missed by a whole-frame average. Inspect the reference before choosing holds. "),
        Some(_) => guidance.push_str("Measured change continues inside detected shots. Inspect entrances, reveals, transformations and exits before choosing edit timings. "),
        None if motion_evidence == "excluded_by_cuts" => guidance.push_str("Motion was analysed, but every valid one-second sample overlaps a cut or its timing uncertainty; within-shot motion cannot be isolated at this sampling rate. "),
        None if motion_evidence == "no_valid_samples" => guidance.push_str("No decoded motion samples are available. Re-analyse the reference and check that its video stream decodes. "),
        None => guidance.push_str("Within-shot motion evidence is unavailable; cut statistics alone cannot establish the reference's visual pace. Re-analyse the reference to populate missing or outdated analysis. "),
    }
    if holds.as_ref().is_some_and(|h| !h.is_empty()) {
        guidance.push_str("Detected settled intervals are inspection candidates, not prescribed hold lengths. ");
    }
    guidance.push_str("Luma differences can include gradual transitions beyond the detector timing window. Luma differences and freeze detection do not identify semantic events or distinguish motion graphics from camera or subject movement. ");
    PacingEvidence {
        within_shot_mean_ydif: within_mean,
        within_shot_sample_seconds: samples.len(),
        valid_sample_seconds: valid,
        cut_exclusion: "boundary bins plus up to 0.5 s before known slow-only detections",
        within_shot_motion_class: motion_class,
        motion_evidence,
        detected_holds: holds,
        hold_evidence: match sheet { None => "unavailable", Some(s) if s.has_freeze => "detected", Some(_) => "no_settled_intervals_detected" },
        guidance,
    }
}

/// Analysis inputs used to derive both the motion summary and pacing evidence.
pub struct StylePacingInput<'a> {
    pub shots: &'a klipp_analysis::shots::Shots,
    pub motion: Option<&'a klipp_analysis::motion::Motion>,
    pub sheet: Option<&'a klipp_analysis::beatsheet::BeatSheet>,
}

/// Assemble the complete StyleProfile response after media analysis and scope
/// measurements. Keeping the returned text, guidance and next step together
/// lets regressions exercise the same response the control handler sends.
pub fn style_profile_response(
    filename: &str,
    media_id: &str,
    cuts: serde_json::Value,
    beats: &klipp_analysis::beats::BeatData,
    look: serde_json::Value,
    analysis: StylePacingInput<'_>,
) -> serde_json::Value {
    let pacing = pacing_evidence(analysis.shots, analysis.motion, analysis.sheet);
    let motion = analysis.motion.and_then(klipp_analysis::motion::Motion::overall_mean).map(|mean| json!({
        "class": klipp_analysis::motion::class_for(mean), "mean": mean,
        "scope": "whole_file_including_cuts",
    }));
    // Guidance: turn the numbers into how to build the same feel.
    let pace = cuts["pace"].as_str().unwrap_or("medium");
    let mean_shot = cuts["mean_shot_secs"].as_f64().unwrap_or(0.0);
    let mut guide = pacing.guidance.clone();
    if beats.confidence > 0.3 {
        let onb = cuts["cut_on_beat_ratio"].as_f64().unwrap_or(0.0);
        if onb > 0.4 {
            guide.push_str(&format!("It cuts on the beat ({:.0}% of cuts) at {:.0} BPM — place your cuts on beat times (klipp_beats on your music). ", onb * 100.0, beats.bpm));
        } else {
            guide.push_str(&format!("The music runs at {:.0} BPM; compare cuts and internal animation accents against the beat. ", beats.bpm));
        }
    }
    if let Some(l) = look.as_object() {
        guide.push_str(&format!("Grade toward the look ({}): blacks ~{} IRE, whites ~{} IRE, saturation ~{}%. Use match_grade against a frame of the reference, or look for a LUT. ", l.get("summary").and_then(|v| v.as_str()).unwrap_or(""), l.get("black_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("white_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("saturation_pct").and_then(|v| v.as_f64()).unwrap_or(0.0)));
    }
    guide.push_str("Match the energy with your own or licensed music, not the reference's track.");

    let text = format!(
        "Style of {filename}:\ndetected cuts — {} shots, mean {:.1} s, {:.0} per minute ({pace} cut cadence){}\nlook — {}\nwhole-file luma change (includes cuts) — {}\nwithin-shot luma change — {}\n\n{guide}",
        cuts["shot_count"].as_i64().unwrap_or(0),
        mean_shot,
        cuts["cuts_per_min"].as_f64().unwrap_or(0.0),
        if beats.confidence > 0.3 { format!("; {:.0} BPM", beats.bpm) } else { String::new() },
        look.get("summary").and_then(|v| v.as_str()).unwrap_or("not measured"),
        motion.as_ref().and_then(|m| m.get("class")).and_then(|v| v.as_str()).unwrap_or("unknown"),
        pacing.within_shot_motion_class.unwrap_or(match pacing.motion_evidence {
            "no_valid_samples" => "no decoded samples",
            "excluded_by_cuts" => "excluded by cuts",
            _ => "unavailable",
        }),
    );
    json!({
        "text": text, "media_id": media_id, "filename": filename,
        "cuts": cuts,
        "tempo": { "bpm": beats.bpm, "confidence": beats.confidence },
        "look": look,
        "motion": motion,
        "guidance": guide,
        "pacing_evidence": pacing,
        "next": if matches!(pacing.motion_evidence, "unavailable" | "no_valid_samples") { "Re-analyse the reference to populate missing or outdated motion analysis, then request style_profile again." } else { "Inspect changes inside the detected shots, including reveals, movement and settled intervals. Choose cut and animation timings from that review; compare accents with the beat and grade toward the measured look using your own footage and music." },
    })
}

/// Where a cut is clean, and where it is not. Folds the measured
/// silences, the transcript's sentence boundaries, and the shot changes
/// into ranked cut opportunities, and — when `near` is given —
/// classifies that moment as clean, mid-sentence, or mid-word, with the
/// nearest clean point. So an agent trims and places on a pause, a
/// full stop or a shot change, and never through the middle of a word
/// or a sentence.
///
/// `words` and `segments` are `(start, end, text)` in file seconds,
/// `silences` are `(start, end)` gaps, `shots` are boundary times.
pub fn cut_points(
    words: &[(f64, f64, String)],
    segments: &[(f64, f64, String)],
    silences: &[(f64, f64)],
    shots: &[f64],
    near: Option<f64>,
    range: (f64, f64),
) -> serde_json::Value {
    let (a, b) = range;
    let in_range = |t: f64| t >= a - 1e-6 && t <= b + 1e-6;
    let ends_sentence = |s: &str| s.trim_end().chars().last().is_some_and(|c| matches!(c, '.' | '?' | '!' | '…'));
    // strength: 3 a silent gap (cleanest), 2 a full stop or a shot cut, 1 a phrase break.
    let mut pts: Vec<(f64, u8, &'static str, String)> = Vec::new();
    for &(s, e) in silences {
        let mid = (s + e) / 2.0;
        if in_range(mid) {
            pts.push((mid, 3, "silence", format!("a {:.2} s pause ({s:.2}-{e:.2})", e - s)));
        }
    }
    for (s, e, text) in segments {
        if ends_sentence(text) {
            if in_range(*e) {
                pts.push((*e, 2, "sentence_end", "a sentence ends".into()));
            }
            if in_range(*s) {
                pts.push((*s, 2, "sentence_start", "a sentence begins".into()));
            }
        } else {
            if in_range(*e) {
                pts.push((*e, 1, "phrase", "a phrase break".into()));
            }
            if in_range(*s) {
                pts.push((*s, 1, "phrase", "a phrase begins".into()));
            }
        }
    }
    for &t in shots {
        if in_range(t) {
            pts.push((t, 2, "shot_change", "the picture cuts".into()));
        }
    }
    // Merge points within a frame of each other, keeping the strongest.
    pts.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f64, u8, &'static str, String)> = Vec::new();
    for p in pts {
        match merged.last_mut() {
            Some(last) if (p.0 - last.0).abs() < 0.05 => {
                if p.1 > last.1 {
                    *last = p;
                }
            }
            _ => merged.push(p),
        }
    }

    // Classify the target moment, if one was asked about.
    let target = near.map(|t| {
        let in_word = words.iter().find(|(s, e, _)| t > *s + 1e-3 && t < *e - 1e-3);
        let in_silence = silences.iter().any(|(s, e)| t >= *s && t <= *e);
        let (verdict, why) = if let Some((_, _, w)) = in_word {
            ("mid-word", format!("the cut lands inside the word \"{w}\" — it would clip the sound"))
        } else if in_silence {
            ("clean", "the cut lands in a pause".into())
        } else if segments.iter().any(|(s, e, _)| t > *s && t < *e) {
            ("mid-sentence", "the cut lands inside a spoken sentence".into())
        } else {
            ("clean", "the cut lands between segments".into())
        };
        // The nearest clean point (silence or sentence end/start) to move to.
        let nearest = merged.iter().filter(|p| p.1 >= 2).min_by(|x, y| (x.0 - t).abs().partial_cmp(&(y.0 - t).abs()).unwrap_or(std::cmp::Ordering::Equal));
        json!({
            "secs": t,
            "verdict": verdict,
            "why": why,
            "nearest_clean_secs": nearest.map(|p| p.0),
            "nearest_clean_delta_secs": nearest.map(|p| p.0 - t),
        })
    });

    // The list the agent reads: nearest first when a target was given.
    let mut ordered: Vec<&(f64, u8, &'static str, String)> = merged.iter().collect();
    if let Some(t) = near {
        ordered.sort_by(|x, y| (x.0 - t).abs().partial_cmp(&(y.0 - t).abs()).unwrap_or(std::cmp::Ordering::Equal));
    }
    let points: Vec<serde_json::Value> = ordered.iter().take(24).map(|(s, strength, kind, why)| json!({
        "secs": s, "kind": kind, "why": why, "strength": strength,
    })).collect();

    let mut text = String::new();
    if let Some(tg) = &target {
        text.push_str(&format!("At {:.2} s: {} — {}\n", near.unwrap_or(0.0), tg["verdict"].as_str().unwrap_or(""), tg["why"].as_str().unwrap_or("")));
        if let Some(n) = tg["nearest_clean_secs"].as_f64() {
            text.push_str(&format!("Nearest clean cut: {:.2} s ({:+.2} s away)\n", n, n - near.unwrap_or(0.0)));
        }
    }
    text.push_str("Clean cut points (strength 3 a pause, 2 a full stop or shot cut, 1 a phrase break):\n");
    for p in &points {
        text.push_str(&format!("  {:.2} s  {}  {}\n", p["secs"].as_f64().unwrap_or(0.0), p["kind"].as_str().unwrap_or(""), p["why"].as_str().unwrap_or("")));
    }
    json!({ "text": text, "target": target, "points": points })
}

/// Public for tests and tools that measure a frame without the socket.
pub fn frame_stats(data: &[u8], width: u32, height: u32, secs: f64) -> serde_json::Value {
    // The picture, not its letterbox or pillarbox bars (#647): counted,
    // a portrait clip in a wide sequence read "mid 0, 76.9% clipped at
    // black", and match_grade lifted it to match those numbers.
    let area = klipp_color::measure::picture_area(data, width, height);
    if area != (0, 0, width, height) {
        let mut v = frame_stats(&klipp_color::measure::crop_rgba(data, width, area), area.2, area.3, secs);
        let note = format!(
            "\nmeasured inside the {} bars: the picture is {}x{} of the {width}x{height} frame",
            if area.2 < width { "pillarbox" } else { "letterbox" },
            area.2,
            area.3
        );
        if let Some(t) = v["text"].as_str().map(|t| format!("{t}{note}")) {
            v["text"] = json!(t);
        }
        v["picture"] = json!({ "x": area.0, "y": area.1, "width": area.2, "height": area.3 });
        return v;
    }
    let (w, h) = (width as usize, height as usize);
    let step = ((w * h) / 200_000).max(1);
    let mut lumas: Vec<f32> = Vec::new();
    let (mut r_sum, mut g_sum, mut b_sum) = (0f64, 0f64, 0f64);
    let (mut sat_sum, mut n) = (0f64, 0usize);
    let (mut clip_hi, mut clip_lo) = (0usize, 0usize);
    let (mut skin_on, mut skin_total) = (0usize, 0usize);
    let mut i = 0;
    while i < w * h {
        let p = i * 4;
        let (r, g, b) = (data[p] as f32, data[p + 1] as f32, data[p + 2] as f32);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        lumas.push(luma / 255.0 * 100.0);
        r_sum += r as f64;
        g_sum += g as f64;
        b_sum += b as f64;
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        sat_sum += sat as f64;
        // At white and black: the luma on the rail, as the measure reads
        // it, an area and not a grain's lone pixel (#459, #518).
        match klipp_color::measure::at_rail(data, w, h, i % w, i / w) {
            Some(klipp_color::measure::Rail::White) => clip_hi += 1,
            Some(klipp_color::measure::Rail::Black) => clip_lo += 1,
            None => {}
        }
        // A pixel bright and warm enough to be skin, and whether its hue
        // sits on the I-line (the vectorscope's skin-tone axis, ~123°):
        // colourists land skin there. Hue from the usual HSV formula.
        if sat > 0.15 && luma > 60.0 && r > g && g >= b {
            let hue = {
                let d = mx - mn;
                let hp = if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
                hp * 60.0
            };
            skin_total += 1;
            if (20.0..=50.0).contains(&hue) {
                skin_on += 1;
            }
        }
        n += 1;
        i += step;
    }
    lumas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |q: f32| lumas.get(((lumas.len() as f32 * q) as usize).min(lumas.len().saturating_sub(1))).copied().unwrap_or(0.0);
    let nn = n.max(1) as f64;
    let (rb, gb, bb) = (r_sum / nn / 255.0, g_sum / nn / 255.0, b_sum / nn / 255.0);
    // The cast, if any: which channel leads on the frame's neutral
    // surfaces and by how much, in percent of their grey level — what
    // "warm" or "cool" means as a number. Read off the neutrals, as the
    // Lab reading and Auto WB are: the whole frame's average is the
    // sky and the brick, and on one frame it called a blue cast while
    // the neutrals leaned warm (#327). `rb`/`gb`/`bb` stay the frame's
    // average colour, which the scene matchers compare.
    let balance_note = match klipp_color::measure::neutral_reference(data, width, height) {
        None => "no neutral surface to read a cast from".to_string(),
        Some(lin) => {
            let [nr, ng, nb] = lin.map(|v| klipp_color::transfer::linear_to_srgb(v) as f64);
            let grey = (nr + ng + nb) / 3.0;
            let cast = |name: &'static str, v: f64| (name, ((v - grey) / grey.max(1e-6) * 100.0) as f32);
            let mut casts = [cast("red", nr), cast("green", ng), cast("blue", nb)];
            casts.sort_by(|a, b| b.1.total_cmp(&a.1));
            if casts[0].1 - casts[2].1 < 4.0 {
                "neutral".to_string()
            } else {
                format!("{} cast, {:.0}% over {}", casts[0].0, casts[0].1 - casts[2].1, casts[2].0)
            }
        }
    };
    let clip_hi_pct = clip_hi as f64 / nn * 100.0;
    let clip_lo_pct = clip_lo as f64 / nn * 100.0;
    let skin_note = if skin_total < (n / 200).max(20) {
        "no clear skin tones".to_string()
    } else {
        format!("{:.0}% of skin-toned pixels sit on the skin-tone line", skin_on as f64 / skin_total as f64 * 100.0)
    };
    let text = format!(
        "Frame at {secs:.2} s, scope readings (luma in IRE, 0 black to 100 white):\n\
         black point {:.0}, mid {:.0}, white point {:.0}\n\
         RGB balance on the neutral surfaces: {balance_note}\n\
         average saturation {:.0}% of full\n\
         clipping: {clip_hi_pct:.1}% at white, {clip_lo_pct:.1}% at black\n\
         skin: {skin_note}",
        pct(0.02), pct(0.5), pct(0.98), sat_sum / nn * 100.0,
    );
    json!({
        "text": text,
        "secs": secs,
        "luma_ire": { "black": pct(0.02), "mid": pct(0.5), "white": pct(0.98) },
        "rgb_balance": { "red": rb, "green": gb, "blue": bb, "note": balance_note },
        "saturation_pct": sat_sum / nn * 100.0,
        "clipping_pct": { "high": clip_hi_pct, "low": clip_lo_pct },
        "skin": skin_note,
    })
}

fn cut_to_budget(text: &str, max_chars: usize) -> (String, bool) {
    cut_to_budget_with(text, max_chars, "ask again with a larger max_chars, or list_clips for the rest")
}

fn cut_to_budget_with(text: &str, max_chars: usize, rest: &str) -> (String, bool) {
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
    kept.push_str(&format!("… cut at {max_chars} chars of {}; {rest}.\n", text.chars().count()));
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
    // The engine's copy runs grade groups baked into their members; an op
    // changes a clip's own grade, and must not copy its group's into it.
    let seq = seq.with_own_grades();
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
}

/// Exact clip changes predicted by resolved cut edits. The UI applies the
/// same EditCommands; callers receive this only after its acknowledgement.
fn planned_cut_changes(ctx: &ControlContext, edits: &[klipp_document::history::EditCommand]) -> Result<serde_json::Value, String> {
    let before = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.with_own_grades().into_owned();
    let mut sequences = vec![before.clone()];
    let mut pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?.clone();
    let mut fps = before.fps;
    {
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for edit in edits { view.apply_forward(edit); }
    }
    let clips = |sequence: &klipp_timeline::Sequence| {
        let fps = sequence.fps.as_f64();
        sequence.tracks.iter().enumerate().flat_map(|(track, lane)| lane.clips.iter().map(move |clip| {
            let id = clip.id.0.to_string();
            let value = json!({
                "clip_id": id, "track": track,
                "timeline_in_frame": (clip.timeline_in.to_secs_f64() * fps).round() as i64,
                "timeline_out_frame": (clip.timeline_out.to_secs_f64() * fps).round() as i64,
                "source_in_secs": clip.source_in.to_secs_f64(), "source_out_secs": clip.source_out.to_secs_f64(),
                "linked_clip_id": clip.linked_clip_id.map(|id| id.0.to_string()),
            });
            (id, value)
        })).collect::<std::collections::BTreeMap<_, _>>()
    };
    let old = clips(&before);
    let new = clips(&sequences[0]);
    let ids = old.keys().chain(new.keys()).cloned().collect::<std::collections::BTreeSet<_>>();
    let mut changes = Vec::new();
    let mut shifted = std::collections::BTreeMap::<(u64, i64), usize>::new();
    for id in ids {
        let prior = old.get(&id);
        let after = new.get(&id);
        if prior == after { continue; }
        if let (Some(prior), Some(after)) = (prior, after) {
            let delta = after["timeline_in_frame"].as_i64().unwrap_or(0)
                - prior["timeline_in_frame"].as_i64().unwrap_or(0);
            if delta != 0
                && prior["track"] == after["track"]
                && after["timeline_out_frame"].as_i64().unwrap_or(0)
                    - prior["timeline_out_frame"].as_i64().unwrap_or(0) == delta
                && prior["source_in_secs"] == after["source_in_secs"]
                && prior["source_out_secs"] == after["source_out_secs"]
                && prior["linked_clip_id"] == after["linked_clip_id"]
            {
                *shifted.entry((prior["track"].as_u64().unwrap_or(0), delta)).or_default() += 1;
                continue;
            }
        }
        changes.push(json!({ "clip_id": id, "before": prior, "after": after,
            "created": prior.is_none(), "removed": after.is_none() }));
    }
    let shifted: Vec<_> = shifted.into_iter().map(|((track, by_frames), clips)| {
        json!({ "track": track, "clips": clips, "by_frames": by_frames })
    }).collect();
    Ok(json!({ "changes": changes, "shifted": shifted }))
}

/// Every reply carries which project it is about: the generation moves
/// when a project is made, opened or saved, and a client that sees it
/// move knows the timeline and the pool it was working on are gone.
fn with_project_generation(ctx: &ControlContext, mut v: serde_json::Value) -> serde_json::Value {
    if let (Some(obj), Ok(p)) = (v.as_object_mut(), ctx.shared.project.lock()) {
        obj.insert("project_generation".into(), json!(p.generation));
        obj.insert("project_identity".into(), json!(p.identity));
    }
    v
}

/// A PNG's size from its header, once the file is whole (it ends in the
/// IEND chunk): a file still being written reads as `None`.
fn png_size_if_whole(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33 || !bytes.starts_with(SIGNATURE) || &bytes[12..16] != b"IHDR" || !bytes.ends_with(&[0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]) {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some((be(16), be(20)))
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
        Some(t.words.into_iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text, confidence: w.confidence }).collect())
    }

    fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).audio.map(|a| a.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default()
    }

    fn shot_changes(&self, item: &klipp_media::MediaItem) -> Vec<f64> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).shots.map(|s| s.boundaries).unwrap_or_default()
    }

    fn motion_between(&self, item: &klipp_media::MediaItem, from: f64, to: f64) -> Option<f32> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).motion?.mean_within(from, to)
    }

    fn library_role(&self, item: &klipp_media::MediaItem) -> Option<klipp_core::ClipRole> {
        match klipp_audio_search::library_source(&item.path)? {
            klipp_audio_search::Source::JamendoMusic => Some(klipp_core::ClipRole::Music),
            klipp_audio_search::Source::Freesound => Some(klipp_core::ClipRole::SFX),
        }
    }

    fn screen_sensitive(&self, item: &klipp_media::MediaItem) -> Vec<klipp_document::ops::SensitiveText> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).screentext.map(|st| st.sensitive.into_iter().map(|s| klipp_document::ops::SensitiveText { kind: s.kind, masked: s.masked, start_secs: s.start_secs, end_secs: s.end_secs, x: s.x, y: s.y, w: s.w, h: s.h }).collect()).unwrap_or_default()
    }

    fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).framing.map(|f| f.samples.iter().flat_map(|s| face_or_head_regions(&f, s).into_iter().map(move |b| (s.secs, b))).collect()).unwrap_or_default()
    }
    fn screen_beats(&self, item: &klipp_media::MediaItem) -> Option<klipp_document::ops::ScreenBeats> {
        let bs = klipp_analysis::report::MediaAnalysis::load(&item.path).beatsheet?;
        if !bs.has_freeze {
            return None;
        }
        Some(klipp_document::ops::ScreenBeats {
            action: bs.segments.iter().filter(|s| s.kind == "action").map(|s| (s.start_secs, s.end_secs)).collect(),
            settles: bs.reveals.iter().copied().chain(bs.cut_points.iter().map(|c| c.secs)).collect(),
            holds: bs.title_windows.iter().map(|w| (w.start_secs, w.end_secs)).collect(),
            spots: bs.spots.iter().map(|s| (s.secs, s.x, s.y)).collect(),
        })
    }
}

/// Whether titles can be set in `family`: bundled with Klipp or
/// installed, matched without case (#738).
fn font_available(family: &str) -> bool {
    let f = family.trim();
    klipp_title::bundled::families().iter().any(|(name, _)| name.eq_ignore_ascii_case(f))
        || klipp_title::font_discovery::discover_system_fonts().keys().any(|k| k.eq_ignore_ascii_case(f))
}

/// What to tell a client that named fonts titles cannot be set in (#738):
/// a brief once took "Montserrat" without a word, and its kickers came out
/// in the fallback.
fn font_warnings<'a>(fonts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for f in fonts.map(str::trim).filter(|f| !f.is_empty()) {
        if !font_available(f) && !seen.iter().any(|s| s.eq_ignore_ascii_case(f)) {
            seen.push(f.to_owned());
        }
    }
    seen.into_iter()
        .map(|f| format!("font \"{f}\" is neither bundled nor installed: titles in it fall back to {}. klipp_fonts lists the fonts there are.", klipp_title::bundled::DEFAULT_FAMILY))
        .collect()
}

/// The fonts the ops in a plan name that cannot be had.
fn op_font_warnings(ops: &[klipp_document::ops::Op]) -> Vec<String> {
    use klipp_document::ops::Op;
    font_warnings(ops.iter().filter_map(|o| match o {
        Op::Title { font: Some(f), .. } | Op::Style { font: Some(f), .. } => Some(f.as_str()),
        _ => None,
    }))
}

/// `warnings` on a reply, and said in its text.
fn with_warnings(v: &mut serde_json::Value, warnings: Vec<String>) {
    if warnings.is_empty() {
        return;
    }
    if let Some(text) = v["text"].as_str() {
        v["text"] = json!(format!("{text}\n{}", warnings.iter().map(|w| format!("[warning] {w}")).collect::<Vec<_>>().join("\n")));
    }
    v["warnings"] = json!(warnings);
}

/// What the timeline holds that the next step would otherwise advise
/// again (#785).
#[derive(Debug, Clone, Copy, Default)]
struct Trail {
    /// The ops placed pictures, not only sound: selects, not effects.
    pictures_placed: bool,
    /// A title is on a text track.
    titled: bool,
    /// A picture clip is graded, alone or in a grade group.
    graded: bool,
}

impl Trail {
    fn read(ctx: &ControlContext, ops: &[klipp_document::ops::Op]) -> Trail {
        let Ok(seq) = ctx.sequence.read() else { return Trail::default() };
        let pictures_placed = ctx.media_pool.read().is_ok_and(|pool| {
            ops.iter().any(|o| match o {
                klipp_document::ops::Op::Place { media_id, .. } => pool.get(media_id).is_some_and(|m| m.is_still || !m.video_codec.is_empty()),
                _ => false,
            })
        });
        let clips = || seq.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (t, c)));
        Trail {
            pictures_placed,
            titled: clips().any(|(t, c)| t.kind == klipp_timeline::TrackKind::Text && matches!(c.kind, klipp_timeline::ClipKind::Title(_))),
            graded: clips().any(|(_, c)| is_picture_clip(c) && (!c.own_grade().is_identity() || c.grade_group.is_some())),
        }
    }
}

/// What to do after a batch of ops was applied, from the ops in it: the
/// reply points at the next step so a session follows a trail rather
/// than recalling a recipe.
/// The brief shapes it (#743): a piece that is not captioned is not told
/// to caption, nor a montage cut to music to cover its joins like an
/// interview's. And the timeline does (#785): sound effects placed are
/// not selects, and titles or a grade already there are not advised.
fn next_after_ops(ops: &[klipp_document::ops::Op], brief: Option<&klipp_timeline::Brief>, trail: Trail) -> Option<String> {
    use klipp_document::ops::Op;
    let has = |f: &dyn Fn(&Op) -> bool| ops.iter().any(f);
    let uncaptioned = brief.is_some_and(|b| !b.captions);
    let selects = trail.pictures_placed && has(&|o| matches!(o, Op::Place { with_audio: true, .. })) && ops.len() >= 2;
    // What is left after the cut, in order, ending in the check.
    let then = || {
        let mut left: Vec<&str> = Vec::new();
        if !trail.titled {
            left.push("titles in the brief's type styles");
        }
        if !trail.graded {
            left.push("the grade");
        }
        match left.as_slice() {
            [] => ", then check.".to_string(),
            [one] => format!(", then {one} and check."),
            many => format!(", then {}, and check.", many.join(", ")),
        }
    };
    let said: &str = if has(&|o| matches!(o, Op::Redact { .. })) {
        "Redacted. Look at a frame inside the stretch (frame_at) to see the box covers what it should — the copy on the track above hides it — then check again."
    } else if selects && uncaptioned && brief.is_some_and(|b| b.music) {
        return Some(format!("The selects are down. Look at them together (frame_at across the cut), cut them to the music's beat (klipp_beats){}", then()));
    } else if selects && uncaptioned {
        return Some(format!("The selects are down. Look at them together (frame_at across the cut) and tighten each cut{}", then()));
    } else if selects {
        "The selects are down. Where two from the same set-up meet is a jump cut: cover the joins with B-roll on a track above (place with_audio false), or j_cut the sound. Then captions, then check."
    } else if has(&|o| matches!(o, Op::Captions { .. })) {
        "Captions are down. Read the cues in the reply and correct any the transcript misheard with set_text. Then check, and export with the brief's loudness."
    } else if has(&|o| matches!(o, Op::LowerThird { .. })) {
        "The lower third is placed. Look at a frame with frame_at to see it sits clear of anything burnt into the picture, then move it with style if not."
    } else if has(&|o| matches!(o, Op::Duck { .. } | Op::Audio { .. })) {
        "The sound is set. Check holds the mix to the craft; then export at the brief's loudness."
    } else if has(&|o| matches!(o, Op::Place { .. })) && !trail.pictures_placed {
        "Placed. Hear it against the picture (audio_excerpt across where it lands), and run check when the cut is together."
    } else if has(&|o| matches!(o, Op::Place { .. })) {
        "Placed. Look with frame_at or contact_sheet, and run check when the cut is together."
    } else {
        "Run check to hold the cut to the brief and the craft before you export."
    };
    Some(said.to_string())
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
/// A sample's faces as boxes. Its people's heads stand in only when no
/// face detector looked; when one did and found no face, there is none
/// to read — the back of a head measured as a face failed every skin
/// check on hair (#326).
fn face_or_head_regions(framing: &klipp_analysis::framing::Framing, s: &klipp_analysis::framing::FrameSample) -> Vec<[f32; 4]> {
    if !s.faces.is_empty() {
        s.faces.iter().map(|f| [f.x, f.y, f.w, f.h]).collect()
    } else if framing.detects_faces() {
        Vec::new()
    } else {
        s.people.iter().map(|p| klipp_color::measure::head_region([p.0, p.1, p.2, p.3])).collect()
    }
}


/// The skin regions to read in a frame: each detected face, its box
/// drawn in a little so hair and background stay out; with no face
/// model, each person's skin cluster, else the head's place in the box.
fn skin_regions(ctx: &ControlContext, secs: f64, data: &[u8], w: u32, h: u32) -> Vec<[f32; 4]> {
    let seq = match ctx.sequence.read() { Ok(s) => s, Err(_) => return Vec::new() };
    let pool = match ctx.media_pool.read() { Ok(p) => p, Err(_) => return Vec::new() };
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    let Some((clip, item)) = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).find_map(|t| {
        let (_, c) = t.clip_at_timecode(tc)?;
        let item = pool.get(&c.media_id()?)?;
        c.enabled.then_some((c.clone(), item.clone()))
    }) else { return Vec::new() };
    let source = clip.source_secs_at((secs - clip.timeline_in.to_secs_f64()).max(0.0), seq.fps.as_f64());
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else { return Vec::new() };
    let Some(sample) = framing.people_at(source, 4.0) else { return Vec::new() };
    if !sample.faces.is_empty() {
        // The cheek band between the eyes and the mouth, which is skin
        // and nothing else; the box drawn in, when the landmarks are no
        // good.
        return sample
            .faces
            .iter()
            .map(|f| klipp_color::measure::skin_band(&f.landmarks).unwrap_or([f.x + f.w * 0.12, f.y + f.h * 0.15, f.w * 0.76, f.h * 0.72]))
            .collect();
    }
    // A face detector looked and saw no face: someone walking away, or
    // too small. There is no skin to read, and the back of a head read
    // as one failed every skin check on hair.
    if framing.detects_faces() {
        return Vec::new();
    }
    sample.people.iter().map(|p| { let p = [p.0, p.1, p.2, p.3]; klipp_color::measure::skin_region(data, w, h, p).unwrap_or_else(|| klipp_color::measure::head_region(p)) }).collect()
}

/// The face's path across a clip, as window keys in clip time.
///
/// A skin key is a band of hue, and a red jumper, a brick wall, a
/// wooden table and a tanned hand all sit inside it — a hue key alone
/// turns every one of them when it turns the face. A window says
/// *where*, and tracking it says where *now*: without the tracking a
/// window is only good for a locked-off camera, and the face walks out
/// of it half way through the shot, taking the correction with it.
///
/// Read from the framing analysis, which already holds face boxes over
/// time, so this costs no inference. Empty when the clip has no
/// analysis or no face in it, and the secondary is then keyed on hue
/// alone as before.
fn face_track(ctx: &ControlContext, clip_id: &str) -> Vec<klipp_document::ops::WindowKey> {
    let Ok(seq) = ctx.sequence.read() else { return Vec::new() };
    let Ok(pool) = ctx.media_pool.read() else { return Vec::new() };
    let Ok(uuid) = clip_id.parse() else { return Vec::new() };
    let id = klipp_core::ClipId(uuid);
    let Some(clip) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id) else { return Vec::new() };
    let Some(item) = clip.media_id().and_then(|m| pool.get(&m)) else { return Vec::new() };
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else { return Vec::new() };
    let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    let fps = seq.fps.as_f64();
    let mut keys: Vec<klipp_document::ops::WindowKey> = framing
        .samples
        .iter()
        .filter(|s| s.secs >= s_in && s.secs < s_out)
        .filter_map(|s| {
            // The biggest face in the sample: the subject. A window on
            // a face in the background would leave the subject's own
            // face outside the correction.
            let b = face_or_head_regions(&framing, s).into_iter().max_by(|a, b| (a[2] * a[3]).total_cmp(&(b[2] * b[3])))?;
            Some(klipp_document::ops::WindowKey { at: clip.elapsed_for_source_secs(s.secs, fps), x: b[0], y: b[1], w: b[2], h: b[3] })
        })
        .collect();
    keys.sort_by(|a, b| a.at.total_cmp(&b.at));
    // A window keyframed every few frames is neither more accurate nor
    // cheaper to read; thirty samples across a shot is a tracker's
    // worth of path.
    if keys.len() > 30 {
        let step = keys.len() as f64 / 30.0;
        keys = (0..30).map(|i| keys[((i as f64 * step) as usize).min(keys.len() - 1)]).collect();
    }
    keys
}

/// A clip's span on the timeline, by its id.
fn shot_span(ctx: &ControlContext, clip_id: &str) -> Option<(f64, f64)> {
    let id = klipp_core::ClipId(clip_id.parse().ok()?);
    let seq = ctx.sequence.read().ok()?;
    seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()))
}

/// A clip read the way a colourist checks a shot: three frames across
/// it, not one. One frame lies — a flash, a passer-by, a cloud — and
/// the drift between them is itself a finding.
/// A clip's detected shots in timeline seconds: the clip cut at the shot
/// boundaries its media's analysis found. Slivers under half a second
/// are dropped; when there are more than `max`, the longest `max` are
/// kept, longest first. A clip with no analysis is one span, itself.
fn clip_shot_spans(clip: &klipp_timeline::Clip, pool: &klipp_media::MediaPool, fps: f64, max: usize) -> Vec<(f64, f64)> {
    let item = clip.media_id().and_then(|m| pool.get(&m));
    let media_fps = item.map(|item| item.fps.as_f64()).filter(|f| *f > 0.0).unwrap_or(fps);
    let boundaries = item
        .and_then(|item| klipp_analysis::report::MediaAnalysis::load(&item.path).shots)
        .map(|s| s.boundaries)
        .unwrap_or_default();
    // The boundaries on the timeline, through the clip's speed and in its
    // direction (#617), each on the first frame that shows the new shot
    // (#691); then cut the span there.
    let from = clip.timeline_in.to_secs_f64();
    let at: Vec<f64> = boundaries.iter().filter_map(|b| clip.first_frame_showing(*b, media_fps, fps)).map(|f| f as f64 / fps).collect();
    shot_spans(from, clip.timeline_out.to_secs_f64(), from, 1.0, &at, max)
}

/// [`clip_shot_spans`] on numbers: the timeline span `from..to` of a
/// clip starting `source_in` into its media at `speed`, cut at the
/// media's shot `boundaries` (source seconds).
fn shot_spans(from: f64, to: f64, source_in: f64, speed: f64, boundaries: &[f64], max: usize) -> Vec<(f64, f64)> {
    let mut cuts: Vec<f64> = boundaries
        .iter()
        .map(|b| from + (b - source_in) / speed)
        .filter(|t| *t > from && *t < to)
        .collect();
    cuts.sort_by(|a, b| a.total_cmp(b));
    let mut edges = vec![from];
    edges.extend(cuts);
    edges.push(to);
    let mut spans: Vec<(f64, f64)> = edges.windows(2).map(|w| (w[0], w[1])).filter(|(a, b)| b - a >= 0.5).collect();
    if spans.is_empty() {
        spans.push((from, to));
    }
    spans.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
    spans.truncate(max.max(1));
    spans
}

#[cfg(test)]
mod status_text_tests {
    use super::*;

    /// #626: what a person is told when they ask whether Klipp is there —
    /// the version, the project by name and whether it is saved, the
    /// timeline, the playhead and the media, in words.
    #[test]
    fn status_reads_as_a_person_would_say_it() {
        let build = klipp_core::build_info::BuildInfo { version: "0.1.0", commit: "f5e7b0ee", dirty: false, profile: "release", exe: None, built_at: None };
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.name = "Main Sequence".into();
        let facts = |seq: &klipp_timeline::Sequence, project: Option<&std::path::Path>, unsaved: bool, media: usize| status_text(&StatusFacts { build: &build, project, unsaved, sequence: seq, media, playhead_secs: 65.2, playing: false });
        let empty = facts(&seq, Some(std::path::Path::new("/home/u/Videos/test it again.klipp")), false, 0);
        assert!(empty.starts_with("Klipp 0.1.0 is running (build f5e7b0ee)."), "{empty}");
        assert!(empty.contains("Project: \u{201c}test it again\u{201d}, all changes saved."), "{empty}");
        assert!(empty.contains("no clips yet") && empty.contains("1920\u{00d7}1080 at 24 fps"), "{empty}");
        assert!(empty.contains("Playhead: paused at 1:05.") && empty.contains("Media: none imported yet."), "{empty}");
        assert!(!empty.contains('/'), "no paths: {empty}");

        seq.tracks[0].clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::Timecode::zero(), klipp_core::Timecode { value: 24 * 90, rate: 24 }, klipp_core::Timecode::zero(), klipp_core::Timecode { value: 24 * 90, rate: 24 }));
        let cut = facts(&seq, None, true, 3);
        assert!(cut.contains("1 clip, 1:30 long") && cut.contains("a new project with changes, not saved yet") && cut.contains("Media: 3 files imported."), "{cut}");
    }
}

#[cfg(test)]
mod shot_span_tests {
    use super::shot_spans;

    /// #329: a clip with a cut in it is two shots, and the longest is
    /// read first — not the clip's middle, which here is past the cut.
    #[test]
    fn a_clip_with_a_cut_is_read_inside_its_longest_shot() {
        // Clip at 23.6..84.1 s of the timeline, from the media's start; the
        // media cuts at 44 s.
        let spans = shot_spans(23.6, 84.1, 0.0, 1.0, &[44.0], 6);
        assert_eq!(spans.len(), 2);
        let (a, b) = spans[0];
        assert!((a - 23.6).abs() < 1e-9 && (b - 67.6).abs() < 1e-9, "the longest shot first: {spans:?}");
        let mid = (a + b) / 2.0;
        assert!(mid < 67.6, "its middle is inside the first shot, not past the cut");
    }

    #[test]
    fn boundaries_outside_the_clip_and_slivers_are_ignored() {
        // Trimmed to source 10..20 s; cuts at 5 (before), 10.2 (a sliver), 15 and 30 (after).
        let spans = shot_spans(0.0, 10.0, 10.0, 1.0, &[5.0, 10.2, 15.0, 30.0], 6);
        let want = [(5.0, 10.0), (0.2, 5.0)];
        assert_eq!(spans.len(), want.len(), "{spans:?}");
        for ((a, b), (x, y)) in spans.iter().zip(want) {
            assert!((a - x).abs() < 1e-9 && (b - y).abs() < 1e-9, "{spans:?}");
        }
    }

    #[test]
    fn many_shots_keep_the_longest_and_none_is_the_whole_clip() {
        let cuts: Vec<f64> = (1..30).map(|i| i as f64 * 10.0 + (i % 3) as f64).collect();
        assert_eq!(shot_spans(0.0, 300.0, 0.0, 1.0, &cuts, 6).len(), 6);
        assert_eq!(shot_spans(5.0, 9.0, 0.0, 1.0, &[], 6), vec![(5.0, 9.0)]);
    }

    #[test]
    fn a_sped_up_clip_maps_its_cuts_onto_the_timeline() {
        // Twice speed from source 0: the cut at source 10 s is 5 s in.
        assert_eq!(shot_spans(0.0, 10.0, 0.0, 2.0, &[10.0], 6), vec![(0.0, 5.0), (5.0, 10.0)]);
    }
}

fn measure_clip(ctx: &ControlContext, from: f64, to: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<(Vec<(f64, klipp_color::measure::FrameMeasure)>, Option<String>), String> {
    let span = (to - from).max(0.05);
    let times: Vec<f64> = if span < 0.6 {
        vec![from + span / 2.0]
    } else {
        [0.2, 0.5, 0.8].iter().map(|f| from + span * f).collect()
    };
    let mut out = Vec::new();
    for t in times {
        match measure_at(ctx, t, only, timeout_ms) {
            Ok(m) => out.push((t, m)),
            Err(e) if out.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    if out.is_empty() {
        return Err("no frame of the clip could be read".into());
    }
    // What moved across the shot: the light, the exposure, the face.
    let spread = |f: &dyn Fn(&klipp_color::measure::FrameMeasure) -> f32| {
        let vs: Vec<f32> = out.iter().map(|(_, m)| f(m)).collect();
        vs.iter().copied().fold(f32::MIN, f32::max) - vs.iter().copied().fold(f32::MAX, f32::min)
    };
    let (d_l, d_a, d_b) = (spread(&|m| m.l_p50), spread(&|m| m.cast_a), spread(&|m| m.cast_b));
    let d_cast = (d_a * d_a + d_b * d_b).sqrt();
    let faces: Vec<f32> = out.iter().filter_map(|(_, m)| m.faces.iter().find(|f| f.readable()).map(|f| f.hue)).collect();
    let d_face = if faces.len() > 1 {
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for w in faces.windows(2) {
            let d = klipp_color::measure::hue_diff(w[0], w[1]);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        hi - lo
    } else {
        0.0
    };
    let mut said = Vec::new();
    if d_l > 6.0 {
        said.push(format!("the exposure moves {d_l:.0} L* across the shot"));
    }
    if d_cast > 4.0 {
        said.push(format!("the light moves {d_cast:.1} in colour across the shot"));
    }
    if d_face > 6.0 {
        said.push(format!("the face's hue moves {d_face:.0}° across the shot"));
    }
    let drift = (!said.is_empty()).then(|| format!("{} — one grade for the whole shot will be wrong at one end; keyframe it or cut it", said.join("; ")));
    Ok((out, drift))
}

/// The middle reading of a clip's frames: what a single grade is set by.
fn median_measure(reads: &[(f64, klipp_color::measure::FrameMeasure)]) -> (f64, klipp_color::measure::FrameMeasure) {
    let mut by_light: Vec<&(f64, klipp_color::measure::FrameMeasure)> = reads.iter().collect();
    by_light.sort_by(|a, b| a.1.l_p50.total_cmp(&b.1.l_p50));
    let mid = by_light[by_light.len() / 2];
    (mid.0, mid.1.clone())
}

/// The frame at a timeline second, measured: Lab readings and the faces.
/// A clip id given as text, for the colour tools' clip-alone reads.
fn clip_of(id: &str) -> Option<klipp_core::ClipId> {
    id.parse().ok().map(klipp_core::ClipId)
}

/// The frame at `secs` measured in Lab — with `only`, that clip alone
/// (#719).
fn measure_at(ctx: &ControlContext, secs: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<klipp_color::measure::FrameMeasure, String> {
    let (data, w, h) = grab_frame_rgba_of(ctx, secs, only, timeout_ms)?;
    let faces = skin_regions(ctx, secs, &data, w, h);
    let mut m = klipp_color::measure::measure_frame(&data, w, h, &faces);
    m.log_prior = log_prior_at(ctx, secs);
    Ok(m)
}

/// What the file of the picture clip at `secs` says about being log.
fn log_prior_at(ctx: &ControlContext, secs: f64) -> klipp_color::measure::LogPrior {
    let (Ok(seq), Ok(pool)) = (ctx.sequence.read(), ctx.media_pool.read()) else { return Default::default() };
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    seq.tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c).filter(|c| is_picture_clip(c)))
        .and_then(|c| pool.get(&c.media_id()?).map(|item| item.log_prior()))
        .unwrap_or_default()
}

/// Whether the colour tools grade this clip: an enabled clip of media.
fn is_picture_clip(c: &klipp_timeline::Clip) -> bool {
    c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. })
}

/// The track the colour tools work on: the top visible video track
/// with a picture clip on it. A track above holding only titles, or
/// nothing, is passed over; taking the first video track as it came
/// left every tool with nothing to grade (#513).
fn picture_track(seq: &klipp_timeline::Sequence) -> Option<&klipp_timeline::Track> {
    seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible && t.clips.iter().any(is_picture_clip))
}

/// `current` with `fields` laid over it by name: what `update_brief`
/// sets. A field given as null goes back to its empty value; a name
/// the brief does not have is refused, where a whole `set_brief` would
/// drop it without a word (#498).
fn merge_brief(current: &klipp_timeline::Brief, fields: &serde_json::Value) -> Result<klipp_timeline::Brief, String> {
    let object = |b: &klipp_timeline::Brief| match serde_json::to_value(b) {
        Ok(serde_json::Value::Object(m)) => Ok(m),
        _ => Err("the brief does not serialise as an object".to_string()),
    };
    let serde_json::Value::Object(fields) = fields else {
        return Err("fields is an object: the brief's fields to change, by name".into());
    };
    let empty = object(&klipp_timeline::Brief::default())?;
    let mut merged = object(current)?;
    for (name, value) in fields {
        let Some(cleared) = empty.get(name) else {
            return Err(format!("{name:?} is not a field of the brief; it has {}", empty.keys().cloned().collect::<Vec<_>>().join(", ")));
        };
        merged.insert(name.clone(), if value.is_null() { cleared.clone() } else { value.clone() });
    }
    protocol::brief_from_value(serde_json::Value::Object(merged))
}

/// The look a frame is judged against: its grade group's when the group
/// has one, or none past faults when the group leaves the look on
/// purpose (#793); else the brief's, else neutral; a screen recording
/// (the beat sheet found holds) is judged as one when the brief names no
/// look.
fn look_for(ctx: &ControlContext, secs: f64) -> klipp_color::measure::LookTarget {
    use klipp_color::measure::LookTarget;
    if let Ok(seq) = ctx.sequence.read() {
        let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
        if let Some(look) = group_look(&seq, tc) {
            return look;
        }
        if let Some(look) = seq.brief.as_ref().and_then(|b| b.look.clone()) {
            return resolve_look_target(look);
        }
        if let Ok(pool) = ctx.media_pool.read() {
            // A screen recording, judged only for cast and clipping. A
            // still picture held on screen also "never changes", and a
            // locked-off camera shot holds too, so the beat sheet alone
            // is not enough: a file with a person in it is footage.
            let item = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).find_map(|t| t.clip_at_timecode(tc)).and_then(|(_, c)| pool.get(&c.media_id()?));
            if let Some(item) = item {
                let a = klipp_analysis::report::MediaAnalysis::load(&item.path);
                let holds = a.beatsheet.as_ref().is_some_and(|b| b.has_freeze);
                let people = a.framing.as_ref().is_some_and(|f| f.samples.iter().any(|s| !s.people.is_empty() || !s.faces.is_empty()));
                if holds && !people {
                    return LookTarget::preset("screencast").unwrap();
                }
            }
        }
    }
    LookTarget::unbriefed()
}

/// The look the grade group of the picture on top at `tc` holds it to:
/// the group's own, or faults only for a group that leaves the look on
/// purpose; `None` when the picture is in no group or its group names
/// neither, and the brief decides (#793).
fn group_look(seq: &klipp_timeline::Sequence, tc: klipp_core::Timecode) -> Option<klipp_color::measure::LookTarget> {
    let clip = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))?;
    let g = seq.grade_groups.iter().find(|g| Some(g.id) == clip.grade_group)?;
    if g.departs {
        return Some(departing_look());
    }
    g.look.clone().map(resolve_look_target)
}

/// What a group that leaves the look on purpose is judged against:
/// faults only. Clipping, crushed blacks, log left ungraded, noise and
/// banding are judged whatever the target, and so is a face lost or
/// blown (skin_lightness). No cast, black point, face exposure or taste
/// numbers: the unbriefed target's neutral cast told a group warm on
/// purpose that its neutrals sat toward orange shot after shot, and its
/// face exposure would fail a section dark on purpose (#811 review).
fn departing_look() -> klipp_color::measure::LookTarget {
    klipp_color::measure::LookTarget { name: "off the look on purpose".into(), ..Default::default() }
}

/// A look given by name alone takes the preset's numbers.
fn resolve_look_target(look: klipp_color::measure::LookTarget) -> klipp_color::measure::LookTarget {
    use klipp_color::measure::LookTarget;
    let bare = look.black_l.is_none() && look.white_l.is_none() && look.mid_l.is_none() && look.contrast_range.is_none() && look.chroma_mean.is_none() && look.cast_a.is_none() && look.skin_hue.is_none();
    if bare {
        if let Some(p) = LookTarget::preset(&look.name) {
            return p;
        }
    }
    look
}

/// The colour findings of a frame as report entries: (severity, secs, what, hint).
fn colour_report(m: &klipp_color::measure::FrameMeasure, look: &klipp_color::measure::LookTarget, secs: f64) -> Vec<(String, f64, String, String)> {
    use klipp_color::measure::Verdict;
    klipp_color::measure::judge(m, look)
        .into_iter()
        .filter(|f| f.verdict != Verdict::Pass)
        .map(|f| ((if f.verdict == Verdict::Fail { "error" } else { "warning" }).to_string(), secs, format!("{} [{}]", f.what, f.rule), f.fix.to_string()))
        .collect()
}

/// The mix metered from `from` to `to` in `step`s: (step start, short-term
/// LUFS at its end, loudest momentary LUFS inside it). Metering starts
/// three seconds early, where there are three, so the first short-term
/// reading has its window (#792).
fn mix_loudness(snapshot: &klipp_render::SequenceSnapshot, from: f64, to: f64, step: f64) -> Result<Vec<(f64, f32, f32)>, String> {
    let fps = snapshot.fps.max(1.0);
    let mut mix = klipp_render::AudioMixdown::new(snapshot).map_err(|e| format!("the mix could not be read: {e}"))?;
    let mut meter = klipp_audio::LufsMeter::new(snapshot.sample_rate as f32, false);
    let first = ((from - 3.0).max(0.0) * fps).floor() as u64;
    let last = (to * fps).ceil() as u64;
    let floor_db = |v: f32| if v.is_finite() { v.max(-70.0) } else { -70.0 };
    let mut rows = Vec::new();
    let mut step_start = from;
    let mut peak = f32::MIN;
    for frame in first..last {
        // Unclamped, as the export's loudness pass meters it (R-3): a hit
        // summed over music is where the mix runs past full scale, and a
        // clamp would read the step that should stand out as quieter (#812).
        let audio = mix.get_frame_audio_unclamped(frame);
        meter.process(&audio);
        let t = (frame + 1) as f64 / fps;
        if t <= from {
            continue;
        }
        let r = meter.readings();
        peak = peak.max(floor_db(r.momentary));
        if t >= step_start + step - 1e-6 || frame + 1 == last {
            rows.push(((step_start * 100.0).round() / 100.0, floor_db(r.short_term), peak));
            step_start += step;
            peak = f32::MIN;
        }
    }
    Ok(rows)
}

/// The level an effect is set to so it sits `over_db` above the loudest
/// sound already heard under it (#792): files a dozen decibels apart, all
/// set to the same volume, left one whoosh shouting over the intro and
/// another lost under the music. Each heard clip under it counts by its
/// file's integrated loudness and its own volume and gain; effects on the
/// SFX track do not count. `None` when either side has not been measured.
fn level_over_bed(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, effect: klipp_core::MediaId, at: f64, secs: f64, over_db: f32) -> Option<f32> {
    let loudness = |id: klipp_core::MediaId| -> Option<f32> {
        let item = pool.get(&id)?;
        let audio = klipp_analysis::report::MediaAnalysis::load(&item.path).audio?;
        // A clip shorter than the meter's gate measures nothing; its
        // true peak, twelve under, stands in for a short hit.
        audio.integrated_lufs.filter(|l| *l > -60.0).or(audio.true_peak_dbtp.map(|p| p - 12.0))
    };
    let bed = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Audio && !t.name.eq_ignore_ascii_case("sfx") && !t.muted)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled && c.timeline_in.to_secs_f64() < at + secs && c.timeline_out.to_secs_f64() > at)
        .filter_map(|c| {
            let id = c.media_id()?;
            let owner = sound_owner(seq, c);
            let db = 20.0 * (owner.volume.max(1e-4) * owner.gain.max(1e-4)).log10();
            Some(loudness(id)? + db)
        })
        .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))?;
    let own = loudness(effect)?;
    Some((bed + over_db - own).clamp(-30.0, 12.0))
}

/// How far grade_to_target may move each knob from where a shot
/// started (#742). Gentle by default: chasing a look's chroma on vivid
/// phone footage took saturation to 1.3x and turned a sky olive.
struct GradeLimits {
    name: &'static str,
    exposure: f32,
    lift: f32,
    gain: f32,
    temperature: f32,
    tint: f32,
    /// Saturation as a factor of the start's: down to 1/x, up to x.
    saturation: f32,
}

impl GradeLimits {
    fn named(name: &str) -> Result<Self, String> {
        let (name, k) = match name.to_lowercase().as_str() {
            "gentle" => ("gentle", 1.0),
            "normal" => ("normal", 2.0),
            "bold" => ("bold", 3.0),
            other => return Err(format!("strength {other:?} is not one of gentle, normal or bold")),
        };
        Ok(Self { name, exposure: 0.5 * k, lift: 0.05 * k, gain: 0.1 * k, temperature: 8.0 * k, tint: 6.0 * k, saturation: 1.0 + 0.12 * k })
    }

    /// The knobs after `step` from `now`, held within the limits around
    /// `start`, and the names of the knobs the limits held.
    fn clamp(&self, start: &klipp_color::measure::PrimaryStep, now: &klipp_color::measure::PrimaryStep, step: &klipp_color::measure::PrimaryStep) -> (klipp_color::measure::PrimaryStep, Vec<&'static str>) {
        let mut held = Vec::new();
        let mut within = |name: &'static str, want: f32, lo: f32, hi: f32| {
            let got = want.clamp(lo, hi);
            if (got - want).abs() > 1e-4 {
                held.push(name);
            }
            got
        };
        let out = klipp_color::measure::PrimaryStep {
            exposure: within("exposure", now.exposure + step.exposure, start.exposure - self.exposure, start.exposure + self.exposure).clamp(-6.0, 6.0),
            lift: within("lift", now.lift + step.lift, start.lift - self.lift, start.lift + self.lift).clamp(-1.0, 1.0),
            gain: within("gain", now.gain + step.gain, start.gain - self.gain, start.gain + self.gain).clamp(-1.0, 1.0),
            temperature: within("temperature", now.temperature + step.temperature, start.temperature - self.temperature, start.temperature + self.temperature).clamp(-50.0, 50.0),
            tint: within("tint", now.tint + step.tint, start.tint - self.tint, start.tint + self.tint).clamp(-50.0, 50.0),
            saturation: within("saturation", now.saturation * (1.0 + step.saturation), start.saturation / self.saturation, start.saturation * self.saturation).clamp(0.0, 4.0),
            held_for_highlights: step.held_for_highlights,
        };
        (out, held)
    }
}

/// A step as the knobs it moves, from `from` to `to`: "exposure +0.30,
/// temperature -6.0, saturation x1.10"; "nothing" when none moves.
fn describe_step(from: &klipp_color::measure::PrimaryStep, to: &klipp_color::measure::PrimaryStep) -> String {
    let mut parts = Vec::new();
    for (name, d, min) in [
        ("exposure", to.exposure - from.exposure, 0.01),
        ("lift", to.lift - from.lift, 0.002),
        ("gain", to.gain - from.gain, 0.002),
        ("temperature", to.temperature - from.temperature, 0.1),
        ("tint", to.tint - from.tint, 0.1),
    ] {
        if d.abs() >= min {
            parts.push(format!("{name} {d:+.2}"));
        }
    }
    if from.saturation > 0.0 && (to.saturation / from.saturation - 1.0).abs() >= 0.005 {
        parts.push(format!("saturation x{:.2}", to.saturation / from.saturation));
    }
    if parts.is_empty() { "nothing".into() } else { parts.join(", ") }
}

/// Shots of one piece of media, two or more, as scenes to grade
/// together: `{file, media_id, clips}` each. Keyed by the media, not the
/// file name, so two cards' IMG_0001.mov are two scenes (#770 review).
fn scene_groups(shots: &[(String, f64, String)], media_of: &std::collections::HashMap<String, klipp_core::MediaId>) -> Vec<serde_json::Value> {
    let mut by_media: Vec<(klipp_core::MediaId, &str, Vec<&str>)> = Vec::new();
    for (id, _, name) in shots {
        let Some(media) = media_of.get(id).copied() else { continue };
        match by_media.iter_mut().find(|(m, _, _)| *m == media) {
            Some(g) => g.2.push(id.as_str()),
            None => by_media.push((media, name.as_str(), vec![id.as_str()])),
        }
    }
    by_media.iter().filter(|(_, _, ids)| ids.len() >= 2).map(|(media, name, ids)| json!({ "file": name, "media_id": media.0.to_string(), "clips": ids })).collect()
}

/// The clip's Corrector knobs as they are: what a grade step adds to.
fn corrector_now(clip: &klipp_timeline::Clip) -> klipp_color::measure::PrimaryStep {
    let mut s = klipp_color::measure::PrimaryStep { saturation: 1.0, ..Default::default() };
    if let Some(n) = clip.own_grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
        s.exposure = n.params.exposure.unwrap_or(0.0);
        s.lift = n.params.lift_master.unwrap_or(0.0);
        s.gain = n.params.gain_master.unwrap_or(0.0);
        s.temperature = n.params.temperature.unwrap_or(0.0);
        s.tint = n.params.tint.unwrap_or(0.0);
        s.saturation = n.params.saturation.unwrap_or(1.0);
    }
    s
}

/// The pool item for a file just imported, waited for: the import is
/// the app's to finish, and it answers before the item is in the pool.
fn imported_id(ctx: &ControlContext, path: &std::path::Path) -> Option<String> {
    imported_id_within(ctx, path, std::time::Duration::from_secs(10))
}

fn imported_id_within(ctx: &ControlContext, path: &std::path::Path, wait: std::time::Duration) -> Option<String> {
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
fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// Seconds since the epoch, for a file name.
fn chrono_stamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}


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

/// What `list_clips` is asked for: every field or the short form, and
/// which clips.
struct ClipFilter {
    full: bool,
    track: Option<serde_json::Value>,
    from_secs: Option<f64>,
    to_secs: Option<f64>,
    media_id: Option<String>,
}

/// The tracks and their clips as `list_clips` answers (#507). By default
/// a clip is its id, kind, range, what it is by name and its media id,
/// with speed, off, graded and effects only where they are so: on 70
/// clips the full form was 63k characters, past what a model is given
/// to read. `full` adds source ranges, frames, blend, grade group and
/// the file's path and log format.
fn list_clips(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, revision: u64, f: &ClipFilter) -> Result<serde_json::Value, String> {
    let fps = seq.fps.as_f64();
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    let want_track = |i: usize, t: &klipp_timeline::Track| match &f.track {
        None => true,
        Some(serde_json::Value::Number(n)) => n.as_u64() == Some(i as u64),
        Some(serde_json::Value::String(s)) => s.parse::<usize>().ok() == Some(i) || t.name.eq_ignore_ascii_case(s.trim()),
        Some(_) => false,
    };
    if f.track.is_some() && !seq.tracks.iter().enumerate().any(|(i, t)| want_track(i, t)) {
        let names: Vec<String> = seq.tracks.iter().enumerate().map(|(i, t)| format!("{i} \"{}\"", t.name)).collect();
        return Err(format!("no track {}; the tracks are {}", f.track.as_ref().map(|v| v.to_string()).unwrap_or_default(), names.join(", ")));
    }
    let want_clip = |c: &klipp_timeline::Clip| {
        let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
        f.from_secs.is_none_or(|from| b > from)
            && f.to_secs.is_none_or(|to| a < to)
            && f.media_id.as_ref().is_none_or(|m| c.media_id().is_some_and(|id| id.0.to_string() == m.trim()))
    };
    let narrowed = f.from_secs.is_some() || f.to_secs.is_some() || f.media_id.is_some();
    let name = |c: &klipp_timeline::Clip| -> Option<String> {
        match &c.kind {
            klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()),
            klipp_timeline::ClipKind::Title(td) => {
                let one_line = td.text().lines().collect::<Vec<_>>().join(" / ");
                let shown: String = one_line.chars().take(40).collect();
                Some(if one_line.chars().count() > 40 { format!("{shown}…") } else { shown })
            }
            klipp_timeline::ClipKind::Generator(g) => format!("{g:?}").split(['{', '(', ' ']).next().map(str::to_string),
            _ => None,
        }
    };
    let clip_json = |c: &klipp_timeline::Clip| -> serde_json::Value {
        if f.full {
            let mut v = json!({
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
                "grade_is_identity": c.own_grade().is_identity(),
                "grade_nodes": c.own_grade().nodes.len(),
                "grade_group": c.grade_group.and_then(|g| seq.grade_group(g)).map(|g| g.name.clone()),
                "media": c.media_id()
                    .and_then(|id| pool.get(&id))
                    .map(|m| json!({
                        "media_id": m.id.0.to_string(),
                        "filename": m.filename,
                        "path": m.path.to_string_lossy(),
                        "log_format": log_format_json(m),
                    })),
            });
            surfaces_json(c, fps, &mut v);
            sound_json(seq, c, true, &mut v);
            return v;
        }
        let mut v = json!({
            "clip_id": c.id.0.to_string(),
            "kind": clip_kind_name(c),
            "in_secs": r3(c.timeline_in.to_secs_f64()),
            "out_secs": r3(c.timeline_out.to_secs_f64()),
        });
        if let Some(n) = name(c) {
            v["name"] = json!(n);
        }
        if let Some(id) = c.media_id() {
            v["media_id"] = json!(id.0.to_string());
        }
        if (c.speed - 1.0).abs() > 1e-6 {
            v["speed"] = json!(c.speed);
        }
        if !c.enabled {
            v["enabled"] = json!(false);
        }
        if !c.own_grade().is_identity() || c.grade_group.is_some() {
            v["graded"] = json!(true);
        }
        let fx = c.builtin_effects.len() + c.effects.len();
        if fx > 0 {
            v["effects"] = json!(fx);
        }
        surfaces_json(c, fps, &mut v);
        sound_json(seq, c, false, &mut v);
        v
    };
    let tracks: Vec<serde_json::Value> = seq
        .tracks
        .iter()
        .enumerate()
        .filter(|(i, t)| want_track(*i, t))
        .filter_map(|(i, t)| {
            let clips: Vec<serde_json::Value> = t.clips.iter().filter(|c| want_clip(c)).map(clip_json).collect();
            if narrowed && clips.is_empty() && f.track.is_none() {
                return None;
            }
            let mut v = json!({ "index": i, "kind": format!("{:?}", t.kind), "name": t.name });
            if f.full {
                v["opacity"] = json!(t.opacity);
                v["locked"] = json!(t.locked);
            } else if t.locked {
                v["locked"] = json!(true);
            }
            v["clips"] = json!(clips);
            // The transitions between the clips listed (#711), in both
            // forms: there are few, and nothing else shows them.
            let transitions: Vec<serde_json::Value> = t
                .transitions
                .iter()
                .filter(|tr| [tr.clip_a_id, tr.clip_b_id].iter().any(|id| t.find_clip(*id).is_some_and(&want_clip)))
                .map(|tr| {
                    let spec = klipp_core::spec_for_kind(tr.kind);
                    let d = tr.duration.to_secs_f64();
                    let mut j = json!({
                        "transition_id": tr.id.0.to_string(),
                        "kind": spec.string_key,
                        "name": spec.display_name,
                        "secs": r3(d),
                        "frames": (d * fps).round() as i64,
                        "alignment": transition_alignment(tr.alignment).1,
                        "clip_a_id": tr.clip_a_id.0.to_string(),
                        "clip_b_id": tr.clip_b_id.0.to_string(),
                    });
                    if let Some((cut, from, to)) = transition_span(t, tr) {
                        j["cut_secs"] = json!(r3(cut));
                        j["from_secs"] = json!(r3(from));
                        j["to_secs"] = json!(r3(to));
                    }
                    j
                })
                .collect();
            if !transitions.is_empty() {
                v["transitions"] = json!(transitions);
            }
            Some(v)
        })
        .collect();
    let mut reply = json!({ "fps": fps, "revision": revision, "tracks": tracks });
    if !f.full {
        reply["note"] = json!("short form: pass full true for source ranges, frames, blend, grade group and file paths; get_grade for a clip's grade");
    }
    Ok(reply)
}

/// Why a request could not be read, in terms an agent can act on (#729).
/// A whole request's parse error points at a byte offset in the line and
/// at serde's names ("untagged enum ClipRef"); a list of ops is read one
/// op at a time instead, so the reply names the step, its op and what
/// went wrong in it, with what a clip can be named by.
fn explain_bad_request(line: &str, e: &serde_json::Error) -> String {
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

/// Why `path` (media `id`) is music, when it is: a track from a library,
/// its credit file beside it, or media whose every clip on the timeline
/// plays in the music role. `None` for anything that may carry a voice.
fn music_reason(ctx: &ControlContext, path: &std::path::Path, id: klipp_core::MediaId) -> Option<&'static str> {
    if klipp_analysis::is_licensed_audio(path) {
        return Some("a library track");
    }
    let seq = ctx.sequence.read().ok()?;
    let roles: Vec<klipp_core::ClipRole> = seq.tracks.iter().flat_map(|t| t.clips.iter())
        .filter(|c| c.media_id() == Some(id))
        .map(|c| sound_owner(&seq, c).role)
        .collect();
    (!roles.is_empty() && roles.iter().all(|r| *r == klipp_core::ClipRole::Music)).then_some("its clips play as music")
}

/// The clip a sound's settings live on: the picture an audio clip is
/// linked to, as the `audio` and `clean_audio` ops keep them, else the
/// clip itself.
fn sound_owner<'a>(seq: &'a klipp_timeline::Sequence, c: &'a klipp_timeline::Clip) -> &'a klipp_timeline::Clip {
    c.linked_clip_id
        .and_then(|id| seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).flat_map(|t| t.clips.iter()).find(|p| p.id == id))
        .unwrap_or(c)
}

/// A clip's sound as set (#740): its level and gain in decibels, pan,
/// fades, how many volume keys it has, its role and its audio effects —
/// what `audio`, `duck`, `role` and `clean_audio` wrote, read back. In
/// the full form for every clip of media; in the brief only what differs
/// from as it came in. A sound linked to a picture keeps its settings on
/// the picture clip, and is read from there on either half.
fn sound_json(seq: &klipp_timeline::Sequence, c: &klipp_timeline::Clip, full: bool, v: &mut serde_json::Value) {
    if !matches!(c.kind, klipp_timeline::ClipKind::Media { .. }) {
        return;
    }
    let c = sound_owner(seq, c);
    let db = |linear: f32| if linear > 0.0 { ((20.0 * linear.log10()) * 10.0).round() / 10.0 } else { -144.0 };
    let mut sound = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value, set: bool| {
        if full || set {
            sound.insert(key.into(), value);
        }
    };
    put("volume_db", json!(db(c.volume)), (c.volume - 1.0).abs() > 1e-4);
    put("gain_db", json!(db(c.gain)), (c.gain - 1.0).abs() > 1e-4);
    put("pan", json!((c.pan * 100.0).round() / 100.0), c.pan.abs() > 1e-4);
    put("fade_in_secs", json!((c.fade_in_secs * 1000.0).round() / 1000.0), c.fade_in_secs > 0.0);
    put("fade_out_secs", json!((c.fade_out_secs * 1000.0).round() / 1000.0), c.fade_out_secs > 0.0);
    put("volume_keys", json!(c.volume_keyframes.len()), !c.volume_keyframes.is_empty());
    put("role", json!(format!("{:?}", c.role).to_lowercase()), c.role != klipp_core::ClipRole::default());
    let effects: Vec<&str> = c.audio_effects.iter().filter(|e| e.enabled).map(|e| e.effect_type.as_str()).collect();
    put("effects", json!(effects), !effects.is_empty());
    if !sound.is_empty() {
        v["sound"] = serde_json::Value::Object(sound);
    }
}

/// A clip's tracked surfaces and what it is pinned to (#684), on its
/// readout: each surface's id and name, and the timeline stretch the
/// track covers, which is where a pin follows it.
fn surfaces_json(c: &klipp_timeline::Clip, fps: f64, v: &mut serde_json::Value) {
    let on_timeline = |src: f64| ((c.timeline_in.to_secs_f64() + c.elapsed_for_source_secs(src, fps)) * 1000.0).round() / 1000.0;
    if !c.surfaces.is_empty() {
        v["surfaces"] = json!(c.surfaces.iter().map(|s| {
            let mut j = json!({ "surface_id": s.id.to_string(), "name": s.name, "frames": s.keys.len() });
            if let Some((a, b)) = s.tracked_secs() {
                let (a, b) = (on_timeline(a), on_timeline(b));
                j["tracked_secs"] = json!([a.min(b), a.max(b)]);
            }
            j
        }).collect::<Vec<_>>());
    }
    if let Some(pin) = c.surface_pin {
        v["pinned_to"] = json!({ "clip_id": pin.host.0.to_string(), "surface_id": pin.surface.to_string() });
    }
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
        klipp_timeline::ClipKind::Precomposition { .. } => "precomposition",
        klipp_timeline::ClipKind::TemplatePrecomposition { .. } => "template_precomposition",
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

/// What [`status_text`] says Klipp is.
struct StatusFacts<'a> {
    build: &'a klipp_core::build_info::BuildInfo,
    project: Option<&'a std::path::Path>,
    unsaved: bool,
    sequence: &'a klipp_timeline::Sequence,
    media: usize,
    playhead_secs: f64,
    playing: bool,
}

/// Minutes and seconds, as a person reads a time: `1:05`, `0:00`; hours
/// when there are any.
fn clock(secs: f64) -> String {
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
fn status_text(f: &StatusFacts) -> String {
    let b = f.build;
    let mut out = format!("Klipp {} is running (build {}", b.version, b.commit);
    if b.newer_build_on_disk() {
        out.push_str("; a newer build is installed, so restart Klipp to use it");
    }
    out.push_str(").\n");
    let name = f.project.and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string());
    out.push_str(&match (name, f.unsaved) {
        (Some(n), false) => format!("Project: \u{201c}{n}\u{201d}, all changes saved.\n"),
        (Some(n), true) => format!("Project: \u{201c}{n}\u{201d}, with unsaved changes.\n"),
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
        "Timeline: \u{201c}{}\u{201d}, {}\u{00d7}{} at {fps} fps, {pictures} picture and {sounds} sound track{}, {contents}.\n",
        seq.name,
        seq.resolution.width,
        seq.resolution.height,
        if sounds == 1 { "" } else { "s" },
    ));
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

/// A bounded server-side wait keeps clients out of poll loops. Each read
/// releases shared locks before sleeping; the UI and other connections run.
fn wait_until<T>(wait: bool, timeout_ms: Option<u64>, mut read: impl FnMut() -> Result<(T, bool), String>) -> Result<(T, bool), String> {
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

fn analysis_reply(ids: &[klipp_core::MediaId], wait: bool, timeout_ms: Option<u64>) -> Result<serde_json::Value, String> {
    let (items, finished) = wait_until(wait, timeout_ms, || {
        let items: Vec<_> = ids.iter().map(|id| klipp_analysis::service::status(*id).ok_or_else(|| format!("analysis {} is no longer available", id.0))).collect::<Result<_, _>>()?;
        let finished = items.iter().all(|s| matches!(s.state, klipp_analysis::service::State::Done | klipp_analysis::service::State::Failed | klipp_analysis::service::State::Cancelled));
        Ok((items, finished))
    })?;
    let succeeded = finished && items.iter().all(|s| s.state == klipp_analysis::service::State::Done);
    Ok(json!({ "items": items, "finished": finished, "succeeded": succeeded, "timed_out": wait && !finished }))
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
/// What the Undo menu calls an `apply` sent without a label: the first
/// op's name, and how many more came with it.
fn apply_label(ops: &[klipp_document::ops::Op]) -> String {
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

fn send_envelope(ctx: &ControlContext, commands: Vec<AppCommand>, batch: bool) -> Result<serde_json::Value, String> {
    send_full_envelope(ctx, commands, Vec::new(), batch, None, None)
}

/// The sound fixes a clip carries, as clean_audio puts them on: the
/// analysis reads the file, so what is already fixed must be known
/// separately (#505).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioFixes {
    wind: bool,
    hiss: bool,
    hum: bool,
}

impl AudioFixes {
    /// A clip that does not play the sound: it asks nothing of it.
    const ALL: Self = Self { wind: true, hiss: true, hum: true };

    fn on(effects: &[klipp_core::AudioEffectInstance]) -> Self {
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
    fn and(self, other: Self) -> Self {
        Self { wind: self.wind && other.wind, hiss: self.hiss && other.hiss, hum: self.hum && other.hum }
    }

    /// Whether the fault `what` (as `AudioEnvelope::faults` words it) is
    /// one these fixes answer.
    fn covers(&self, what: &str) -> bool {
        (what.contains("hum") && self.hum)
            || ((what.contains("rumble") || what.contains("wind")) && self.wind)
            || ((what.contains("hiss") || what.contains("noise floor")) && self.hiss)
    }
}

/// grade_to_target's undo label: its steps carry it, and are folded
/// into one under it.
const GRADE_TO_TARGET: &str = "Grade To Target";

/// The undo steps of a tool that grades in rounds, measuring between
/// them: sent one by one under `label`, since each has to be applied to
/// be measured, and folded into one when the tool is done. A pass over
/// 31 shots was a hundred ctrl+z (#514).
struct UndoFold {
    label: &'static str,
    steps: usize,
    last_revision: Option<u64>,
}

impl UndoFold {
    fn new(label: &'static str) -> Self {
        Self { label, steps: 0, last_revision: None }
    }

    /// Apply `edits`, resolved at `revision`, as one of the pass's steps.
    fn send(&mut self, ctx: &ControlContext, edits: Vec<klipp_document::history::EditCommand>, revision: u64) -> Result<serde_json::Value, String> {
        let sent = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(self.label.into()))?;
        self.steps += 1;
        self.last_revision = sent.get("revision").and_then(|r| r.as_u64());
        Ok(sent)
    }

    /// Fold the pass's steps into one. The UI does it only if nothing
    /// else moved the document meanwhile, and never over an edit of the
    /// person's. The undo steps the pass is now.
    fn finish(&self, ctx: &ControlContext) -> usize {
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

fn send_full_envelope(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    expected_revision: Option<u64>,
    label: Option<String>,
) -> Result<serde_json::Value, String> {
    send_full_envelope_for_project(ctx, commands, edits, batch, expected_revision, None, label)
}

fn send_full_envelope_for_project(
    ctx: &ControlContext,
    commands: Vec<AppCommand>,
    edits: Vec<klipp_document::history::EditCommand>,
    batch: bool,
    expected_revision: Option<u64>,
    expected_project_identity: Option<u64>,
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
    let background_job = if commands.iter().any(|c| matches!(c, AppCommand::ImportMedia { .. } | AppCommand::ImportMediaWithAnalysis { .. })) {
        Some("import_probe")
    } else if commands.iter().any(|c| matches!(c, AppCommand::ExportSequence { .. })) {
        Some("export")
    } else {
        None
    };
    let operation_id = operations::queued(background_job);
    ctx.commands
        .send(ControlEnvelope { commands, edits, batch, expected_revision, expected_project_identity, label, ack: Some(tx) })
        .map_err(|e| {
            operations::finished(&operation_id, ctx.shared.revision(), Some("app command channel closed".into()));
            format!("app command channel closed: {e}")
        })?;
    (ctx.wake_ui)();
    match rx.recv_timeout(ACK_TIMEOUT) {
        Ok(ack) => {
            operations::finished(&operation_id, ack.revision, ack.refused.clone());
            if let Some(why) = ack.refused {
                return Err(format!("not applied (operation {operation_id}): {why}"));
            }
            Ok(json!({
                "operation_id": operation_id,
                "applied": true,
                "accepted": true,
                "commands": count,
                "revision": ack.revision,
                "generation": generation,
            }))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let pending_id = operation_id.clone();
            let shared = Arc::clone(&ctx.shared);
            std::thread::spawn(move || match rx.recv() {
                Ok(ack) => operations::finished(&pending_id, ack.revision, ack.refused),
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

/// What a batch gathers: the writes that are app commands, and the ones
/// that are document ops (a look), resolved together when it is sent.
#[derive(Default)]
struct Collected {
    commands: Vec<AppCommand>,
    ops: Vec<klipp_document::ops::Op>,
}

/// `collect` is `Some` inside a batch: writes are gathered there instead
/// of being sent, so the batch goes to the UI whole or not at all.
fn handle(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    let in_batch = collect.is_some();
    let mut send = |c: AppCommand| -> Result<serde_json::Value, String> {
        if let Some(buf) = collect.as_mut() {
            buf.commands.push(c);
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
            list_clips(&seq, &pool, ctx.shared.revision(), &ClipFilter { full, track, from_secs, to_secs, media_id })
        }

        ControlCommand::Analyze { media_id, download_models, wait, timeout_ms } => {
            if in_batch {
                return read_in_batch("analyze");
            }
            let downloaded = if download_models { klipp_analysis::download_models()? } else { Vec::new() };
            // How the brief spells things goes to the transcriber; a
            // transcript made without those spellings is made again.
            let spellings = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.as_ref().map(|b| b.spellings()).unwrap_or_default();
            klipp_analysis::transcript::set_vocabulary(spellings);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let items: Vec<&klipp_media::MediaItem> = match media_id {
                Some(id) => {
                    let id = klipp_core::MediaId(id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
                    vec![pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?]
                }
                None => pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File && !m.offline).collect(),
            };
            let ids: Vec<_> = items.iter().map(|m| m.id).collect();
            let statuses: Vec<klipp_analysis::service::Status> = items.into_iter().map(klipp_analysis::service::request).collect();
            drop(pool);
            if wait {
                let mut reply = analysis_reply(&ids, true, timeout_ms)?;
                reply["models_downloaded"] = json!(downloaded);
                return Ok(reply);
            }
            let all_done = statuses.iter().all(|s| matches!(s.state, klipp_analysis::service::State::Done));
            let next = if all_done {
                "Already analysed. Read a transcript (with words) to choose the selects, and find_shots with no people for B-roll."
            } else {
                "Analysis runs in the background — shots, sound, motion, the transcript, framing. Watch analysis_status; a transcript of a long file is about a third of its length. Read each file's transcript and shots when it is done."
            };
            Ok(json!({ "items": statuses, "models_downloaded": downloaded, "next": next }))
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

        ControlCommand::ScreenText { media_id, from_secs, to_secs, all } => {
            if in_batch {
                return read_in_batch("screen_text");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(st) = report.screentext else {
                let installed = klipp_analysis::screentext::installed();
                let enabled = klipp_analysis::screen_text_enabled();
                return Ok(json!({
                    "text": format!("{}: no screen text read yet. {}", item.filename, if !enabled { "Screen text is switched off in preferences." } else if !installed { "The OCR models are not downloaded: analyze with download_models true (about 10 MB, the user should know), then read it again." } else { "Run analyze, then read it again." }),
                    "installed": installed,
                    "enabled": enabled,
                }));
            };
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(st.duration_secs));
            let within = st.spans_within(a, b);
            // The summary: what stayed up a second or more, or reads as a
            // heading — short, mostly letters, read with confidence — the
            // sidebar and the menu bar once each; not every line of body
            // text in every settings page.
            let heading_like = |s: &klipp_analysis::screentext::TextSpan| s.confidence >= 0.85 && s.text.split_whitespace().count() <= 6 && s.text.chars().filter(|c| c.is_alphabetic()).count() * 10 >= s.text.chars().count() * 7;
            let chosen: Vec<&klipp_analysis::screentext::TextSpan> = if all { within.clone() } else { within.iter().copied().filter(|s| s.end_secs - s.start_secs >= 1.0 || heading_like(s)).take(80).collect() };
            let spans: Vec<serde_json::Value> = chosen.iter().map(|s| json!({ "text": s.text, "start_secs": s.start_secs, "end_secs": s.end_secs, "x": s.x, "y": s.y, "w": s.w, "h": s.h, "confidence": s.confidence })).collect();
            let mut text = format!("{}  on-screen text {:.2}-{:.2} s ({} of {} lines{}, {} frames read)\n", item.filename, a, b, chosen.len(), within.len(), if all || chosen.len() == within.len() { "" } else { "; all true for every line" }, st.frames_read);
            for s in chosen.iter().take(if all { 400 } else { 80 }) {
                text.push_str(&format!("[{:.2}-{:.2}] ({:.2},{:.2}) {}\n", s.start_secs, s.end_secs, s.x, s.y, s.text));
            }
            for s in st.sensitive.iter().filter(|s| s.end_secs > a && s.start_secs < b) {
                text.push_str(&format!("SENSITIVE {} {:.2}-{:.2} s at ({:.2},{:.2}): {}\n", s.kind, s.start_secs, s.end_secs, s.x, s.y, s.masked));
            }
            Ok(json!({
                "text": text,
                "media_id": media_id,
                "spans": spans,
                "sensitive": st.sensitive,
                "frames_read": st.frames_read,
                "next": "Cut to where the text that matters is up and hold until it reads; a SENSITIVE line must be blurred or cut before delivery (check says so too).",
            }))
        }

        ControlCommand::SearchScreenText { query, max_hits } => {
            if in_batch {
                return read_in_batch("search_screen_text");
            }
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let mut hits = Vec::new();
            let max = max_hits.unwrap_or(50);
            'files: for m in pool.all() {
                let report = klipp_analysis::report::MediaAnalysis::load(&m.path);
                let Some(st) = report.screentext.as_ref() else { continue };
                for s in st.search(&query) {
                    hits.push(json!({ "media_id": m.id.0.to_string(), "filename": m.filename, "start_secs": s.start_secs, "end_secs": s.end_secs, "text": s.text, "x": s.x, "y": s.y, "w": s.w, "h": s.h }));
                    if hits.len() >= max {
                        break 'files;
                    }
                }
            }
            Ok(json!({ "query": query, "hits": hits }))
        }

        ControlCommand::AutoRedact { apply } => {
            if in_batch {
                return Err("auto_redact is its own batch; send it on its own".into());
            }
            use klipp_document::ops::{Anchor, ClipRef, Op, Transcripts as _};
            let ops: Vec<Op> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let mut ops = Vec::new();
                let mut cache: std::collections::HashMap<klipp_core::MediaId, Vec<klipp_document::ops::SensitiveText>> = std::collections::HashMap::new();
                for t in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
                    for c in t.clips.iter().filter(|c| c.enabled) {
                        let Some(media_id) = c.media_id() else { continue };
                        let Some(item) = pool.get(&media_id) else { continue };
                        let found = cache.entry(media_id).or_insert_with(|| AnalysedTranscripts.screen_sensitive(item));
                        let (s_in, s_out, t_in) = (c.source_in.to_secs_f64(), c.source_out.to_secs_f64(), c.timeline_in.to_secs_f64());
                        for s in found.iter().filter(|s| s.end_secs > s_in && s.start_secs < s_out) {
                            let from = t_in + (s.start_secs.max(s_in) - s_in);
                            let to = t_in + (s.end_secs.min(s_out) - s_in);
                            ops.push(Op::Redact { clip_id: ClipRef::Id(c.id), from: Some(Anchor::Secs(from)), to: Some(Anchor::Secs(to)), x: s.x, y: s.y, w: s.w, h: s.h, kind: "mosaic".into(), amount: None });
                        }
                    }
                }
                ops
            };
            if ops.is_empty() {
                return Ok(json!({ "text": "Nothing to redact: the screen text found nothing sensitive in the stretches the timeline shows (or it has not been read: analyze with the OCR models).", "steps_resolved": 0, "applied": false }));
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Auto Redact".into()))?;
                v["next"] = json!("Redacted. Look at a frame inside each stretch (frame_at) to see the box covers the text, then check again.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what auto_redact would hide. Run it with apply true to do it as one undo step." })
            };
            v["text"] = json!(plan.text());
            v["steps_resolved"] = json!(plan.steps.len());
            v["ops"] = json!(ops);
            Ok(v)
        }

        ControlCommand::AutoSfx { whoosh, click, riser, on, apply } => {
            if in_batch {
                return Err("auto_sfx is its own batch; send it on its own".into());
            }
            let on = on.as_deref().map(klipp_document::sound_design::SfxOn::parse).transpose()?.unwrap_or_default();
            let synthesized = whoosh.is_none() && click.is_none() && riser.is_none();
            if synthesized && !apply {
                let seq = ctx.sequence.read().unwrap_or_else(|e| e.into_inner());
                let placements = klipp_document::sound_design::preview_whoosh(&seq, f64::from(klipp_audio::WhooshVariant::Short.duration_secs()), on);
                let summary: Vec<_> = placements.iter().map(|p| format!("whoosh at {:.2} s", p.at_secs)).collect();
                return Ok(json!({ "text": format!("Would synthesize a short whoosh and place {} sounds: {}. Run auto_sfx with apply true to create the audio and place it.", summary.len(), summary.join("; ")), "placements": summary, "applied": false, "synthesized_whoosh": false }));
            }
            let whoosh = if synthesized {
                // Materialize and import the reusable asset only when applying.
                // Serializing this small path
                // avoids two socket clients writing/importing the same WAV.
                static SYNTH_IMPORT: Mutex<()> = Mutex::new(());
                let _guard = SYNTH_IMPORT.lock().unwrap_or_else(|e| e.into_inner());
                let dir = klipp_media::made::made_dir("auto-sfx");
                std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
                let path = dir.join("whoosh-short-v1.wav");
                if !path.is_file() {
                    let staging = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
                    let saved = klipp_audio::write_whoosh_wav(klipp_audio::WhooshVariant::Short, &staging)
                        .and_then(|_| std::fs::rename(&staging, &path));
                    if let Err(e) = saved {
                        let _ = std::fs::remove_file(&staging);
                        return Err(format!("synthesize whoosh: {e}"));
                    }
                }
                let imported = handle(ctx, ControlCommand::ImportMedia { path: path.to_string_lossy().into_owned(), analyze: false }, &mut None)?;
                let id = imported["media_id"].as_str().ok_or("the whoosh was synthesized but its import has not completed")?;
                Some(klipp_core::MediaId(parse_uuid(id, "media_id")?))
            } else { whoosh };
            let (placements, ops) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                for (name, id) in [("whoosh", whoosh), ("click", click), ("riser", riser)] {
                    if let Some(id) = id {
                        let item = pool.get(&id).ok_or_else(|| format!("{name}: no media item {}; media ids are in list_media", id.0))?;
                        if !item.has_audio {
                            return Err(format!("{name}: {} has no sound", item.filename));
                        }
                    }
                }
                let reveals = |id: klipp_core::MediaId| pool.get(&id).and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).beatsheet).map(|b| b.reveals).unwrap_or_default();
                let loudest = |id: klipp_core::MediaId| if synthesized && Some(id) == whoosh { Some(0.15) } else { pool.get(&id).and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).audio).and_then(|a| a.loudest_secs) };
                let kit = klipp_document::sound_design::SfxKit { whoosh, click, riser, on };
                let mut placements = klipp_document::sound_design::placements(&seq, &pool, kit, &reveals, &loudest);
                // Each effect against what plays under it, not at one level
                // for files a dozen decibels apart (#792): a whoosh a little
                // over the bed, a click a little under, a riser level.
                for p in placements.iter_mut() {
                    let over = match p.what { "whoosh" => 3.0, "click" => -3.0, _ => 0.0 };
                    if let Some(v) = level_over_bed(&seq, &pool, p.media_id, p.at_secs, p.secs, over) {
                        p.volume_db = v;
                    }
                }
                let ops = klipp_document::sound_design::ops(&seq, &placements);
                (placements, ops)
            };
            if ops.is_empty() {
                return Ok(json!({ "text": match on {
                    klipp_document::sound_design::SfxOn::Cuts => "Nothing to place: no cut on the top picture track without an effect there already, and no reveal inside its clips (analyze gives the beat sheet).",
                    klipp_document::sound_design::SfxOn::Transitions => "Nothing to place: no cut on the top picture track carries a transition without an effect there already.",
                    klipp_document::sound_design::SfxOn::Stylised => "Nothing to place: no cut on the top picture track carries a stylised transition (zoom, whip, glitch, film burn…) without an effect there already.",
                }, "steps_resolved": 0, "applied": false }));
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let summary: Vec<String> = placements.iter().map(|p| format!("{} at {:.2} s", p.what, p.at_secs)).collect();
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Auto SFX".into()))?;
                v["next"] = json!("Placed. Play across a cut to hear the whoosh land on it; check reads the levels.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what auto_sfx would place. Run it with apply true to do it as one undo step." })
            };
            v["text"] = json!(format!("{}\n{}", summary.join("; "), plan.text()));
            v["placements"] = json!(summary);
            v["synthesized_whoosh"] = json!(synthesized);
            v["steps_resolved"] = json!(plan.steps.len());
            Ok(v)
        }

        ControlCommand::MixLoudness { from_secs, to_secs, step_secs } => {
            if in_batch {
                return read_in_batch("mix_loudness");
            }
            let snapshot = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear)
                    .map_err(|e| format!("the mix could not be read: {e}"))?
            };
            let end = snapshot.end_frame() as f64 / snapshot.fps.max(1.0);
            let from = from_secs.unwrap_or(0.0).clamp(0.0, end);
            let to = to_secs.unwrap_or(end).clamp(from, end);
            let step = step_secs.unwrap_or(0.5).clamp(0.1, 10.0);
            if to - from < step {
                return Err(format!("{from:.2}-{to:.2} s is shorter than one step of {step:.2} s"));
            }
            let rows = mix_loudness(&snapshot, from, to, step)?;
            let bed = {
                let mut st: Vec<f32> = rows.iter().map(|r| r.1).filter(|v| v.is_finite() && *v > -70.0).collect();
                st.sort_by(f32::total_cmp);
                st.get(st.len() / 2).copied()
            };
            let lines: Vec<String> = rows
                .iter()
                .map(|(t, short, peak)| {
                    let rise = bed.is_some_and(|b| *peak > b + 4.0);
                    format!("{t:7.2} s  short-term {short:6.1} LUFS  loudest 400 ms {peak:6.1}{}", if rise { "  ← stands out" } else { "" })
                })
                .collect();
            Ok(json!({
                "text": format!(
                    "The mix from {from:.1} to {to:.1} s, metered every {step:.2} s as the export mixes it, before loudness normalisation{}:\n{}",
                    bed.map(|b| format!(" (median short-term {b:.1} LUFS; a step whose loudest 400 ms is 4 dB over that stands out)")).unwrap_or_default(),
                    lines.join("\n"),
                ),
                "from_secs": from, "to_secs": to, "step_secs": step,
                "median_short_term_lufs": bed,
                "steps": rows.iter().map(|(t, s, p)| json!({ "secs": t, "short_term_lufs": s, "momentary_max_lufs": p })).collect::<Vec<_>>(),
                "next": "A whoosh or a hit should stand out by a few dB at its cut; an effect that does not is buried. audio sets a clip's level; mix_loudness again to hear the change.",
            }))
        }

        ControlCommand::AudioSpectrum { media_id, from_secs, to_secs } => {
            if in_batch {
                return read_in_batch("audio_spectrum");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, dur, has_audio) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64(), item.has_audio)
            };
            if !has_audio {
                return Err(format!("{filename} has no sound in it"));
            }
            let from = from_secs.unwrap_or(0.0).max(0.0);
            let to = to_secs.filter(|t| *t > from);
            // Enough columns that a fault a second long is its own
            // column on anything up to an hour, and the cost is the
            // same whatever the file's length.
            let spec = klipp_analysis::spectrogram::of_file(std::path::Path::new("ffmpeg"), &path, from, to, 600, 128)?;
            // Music has no voice for the noise rules to measure against
            // (#723): its bass read as mains hum "under the voice". A
            // track from a library (its credit beside it), or media every
            // clip of plays as music, is not judged by them.
            let music = music_reason(ctx, &path, id);
            let found = if music.is_some() { Vec::new() } else { klipp_analysis::spectrogram::findings(&spec) };
            let lines: Vec<String> = found.iter().map(|(sev, what, fix)| format!("[{sev}] {what} — {fix}")).collect();
            // The bands as numbers, thinned so a reply stays readable:
            // the loudest column of each second.
            let per_second = (1.0 / spec.hop_secs.max(0.001)).round().max(1.0) as usize;
            let bands: Vec<serde_json::Value> = spec
                .bands
                .iter()
                .map(|b| {
                    let thinned: Vec<f32> = b.db.chunks(per_second).map(|c| c.iter().fold(-96.0f32, |a, v| a.max(*v))).collect();
                    json!({ "name": b.name, "low_hz": b.low_hz, "high_hz": b.high_hz, "db_per_second": thinned })
                })
                .collect();
            Ok(json!({
                "text": format!(
                    "The sound of {filename}{}, {:.1} s read in {:.2} s columns:\n{}{}",
                    if from > 0.0 || to.is_some() { format!(" from {:.1} s", from) } else { String::new() },
                    spec.duration_secs,
                    spec.hop_secs,
                    match &music {
                        Some(why) => format!("This is music ({why}): the noise rules measure hum, rumble, hiss and sibilance against a voice, and music has none to measure against; its low end is its bass. The bands below show its balance."),
                        None if lines.is_empty() => "Nothing to report: no band comes close enough to the voice to be heard under it.".to_string(),
                        None => lines.join("\n"),
                    },
                    if music.is_some() { "" } else { "\n\nEvery threshold here is a level *relative to the voice*, never an absolute one: a quiet recording of a clean room and a loud recording of a noisy one have the same absolute noise floor and different problems. Under about 30 dB below the voice a steady noise stops being audible under speech; at 20 dB down it is plainly there." },
                ),
                "media_id": media_id,
                "filename": filename,
                "duration_secs": spec.duration_secs,
                "media_duration_secs": dur,
                "hop_secs": spec.hop_secs,
                "rate": spec.rate,
                "findings": found.iter().map(|(sev, what, fix)| json!({ "severity": sev, "what": what, "fix": fix })).collect::<Vec<_>>(),
                "music": music.is_some(),
                "bands": bands,
                "next": if music.is_some() {
                    "For music, audio sets its level and duck lowers it under speech; the dialogue cleaners are for recorded sound."
                } else {
                    "clean_audio on the clip whose sound plays takes voice (Voice Isolation, for dialogue), wind, hiss, hum and deess; run audio_spectrum again to see whether it worked."
                },
            }))
        }

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
                    "text": format!("The look of {source}: {describe}. Nearest preset: {}. Run it with apply true to make it the target every colour judgement is held to.", nearest.as_ref().map(|(n, d)| format!("{n} ({d:.0} away)")).unwrap_or_else(|| "none".into())),
                    "look_target": target,
                    "source": source,
                    "nearest_preset": nearest.as_ref().map(|(n, _)| n.clone()),
                    "applied": false,
                }));
            }
            let mut brief = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone().unwrap_or_default();
            brief.look = Some(target.clone());
            let mut v = handle(ctx, ControlCommand::SetBrief { brief }, &mut None)?;
            v["text"] = json!(format!("Every colour judgement is now held to the look of {source}: {describe}. check, grade_to_target and match_scenes all read it from the brief."));
            v["look_target"] = serde_json::to_value(&target).unwrap_or(serde_json::Value::Null);
            v["source"] = json!(source);
            v["applied"] = json!(true);
            Ok(v)
        }

        ControlCommand::MatchScenes { apply, rounds } => {
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
                return Err("matching needs at least two shots on one video track".into());
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
                let hero_name: String = shots[hero].3.chars().take(24).collect();
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
                        "  {name} at {:.2} s: {verdict}{}",
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
                    let (a, b): (String, String) = (shots[w[0].0].3.chars().take(18).collect(), shots[w[1].0].3.chars().take(18).collect());
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
                let name: String = c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into()).chars().take(28).collect();
                let chain = c.own_grade().chain();
                let faults = c.own_grade().chain_out_of_order();
                let shown: Vec<String> = chain.iter()
                    .filter(|l| !matches!(l.stage, klipp_nodes::GradeStage::Plumbing) || l.kind == klipp_nodes::NodeKind::Source)
                    .map(|l| format!("{} [{}]{}{}", l.label, l.stage.label(), if l.gated { ", gated by a key" } else { "" }, if l.enabled { "" } else { ", bypassed" }))
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
            let (data, fw, fh) = grab_frame_rgba(ctx, secs, timeout_ms)?;
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
                let name = item.map(|it| it.filename.clone()).unwrap_or_else(|| "the clip".into());
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

        ControlCommand::GradeToTarget { clip_id, apply, rounds, force, strength } => {
            if in_batch {
                return Err("grade_to_target measures and applies; send it on its own".into());
            }
            let limits = GradeLimits::named(strength.as_deref().unwrap_or("gentle"))?;
            let shots: Vec<(String, f64, String)> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let want = clip_id.as_deref().map(|s| s.parse::<uuid::Uuid>().map_err(|e| format!("clip_id is not a uuid: {e}"))).transpose()?;
                // One clip asked for is found on whichever video track
                // holds it; the whole pass takes the picture track.
                // Every visible video track, the asked-for clip or all of
                // them: the top one alone left the main picture under an
                // overlay ungraded (#720). Each is measured alone (#719).
                let tracks: Vec<&klipp_timeline::Track> =
                    seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).collect();
                tracks.into_iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| is_picture_clip(c) && want.is_none_or(|w| c.id.0 == w))
                    .map(|c| (c.id.0.to_string(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0, c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into())))
                    .take(48)
                    .collect()
            };
            if shots.is_empty() {
                return Err(match &clip_id {
                    Some(id) => format!("{id} is not a picture clip on a visible video track"),
                    None => "no picture clip to grade on a video track".into(),
                });
            }
            let n_rounds = if apply { rounds.unwrap_or(3).clamp(1, 5) } else { 0 };
            let mut fold = UndoFold::new(GRADE_TO_TARGET);
            let mut per_shot = Vec::new();
            let mut lines = Vec::new();
            for (id, mid, name) in &shots {
                let look = look_for(ctx, *mid);
                // Three frames across the shot, not one: the middle
                // reading grades it and the drift between them is said.
                let span = shot_span(ctx, id).unwrap_or((*mid - 0.5, *mid + 0.5));
                let (reads, drift) = measure_clip(ctx, span.0, span.1, clip_of(id), Some(6000))?;
                let (mid_t, mut m) = median_measure(&reads);
                let mid = &mid_t;
                // Log with no input transform is not a grading problem.
                if klipp_color::measure::looks_like_log(&m) && !force {
                    let short: String = name.chars().take(28).collect();
                    lines.push(format!("{short} at {mid:.2} s: left alone — it looks like camera log with no input transform (5–95 range {:.0}, chroma {:.0}). Set the clip's log format so the transform runs, then grade; grade_to_target with force true grades it as it is.", m.contrast_range, m.chroma_mean));
                    per_shot.push(json!({ "clip_id": id, "secs": mid, "skipped": "looks like camera log with no input transform", "lab": m }));
                    continue;
                }
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
                    // A round that leaves the shot worse off is not a
                    // step toward anything. The commonest case is a
                    // cast the primaries cannot reach — a tungsten shot
                    // against a daylight target — where the loop pushes
                    // exposure around for ever, crushes the blacks, and
                    // never moves the cast at all. Put the knobs back
                    // where they were and say why it stopped.
                    let after_round = colour_report(&m, &look, *mid);
                    let turned = klipp_color::measure::memory_colours_broken(&start_m, &m);
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
                let short: String = name.chars().take(28).collect();
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
                    look.name,
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
            let media_of: std::collections::HashMap<String, klipp_core::MediaId> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                seq.tracks.iter().flat_map(|t| t.clips.iter()).filter_map(|c| c.media_id().map(|m| (c.id.0.to_string(), m))).collect()
            };
            let groups = scene_groups(&shots, &media_of);
            for g in &groups {
                lines.push(format!(
                    "{} shots come from {}: one scene. Grade them together (grade_group with these clips, then grade on the group) and keep per-shot moves small; a primary each, chasing the target alone, pulls them apart at the cuts.",
                    g["clips"].as_array().map_or(0, |a| a.len()),
                    g["file"].as_str().unwrap_or(""),
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
            Ok(json!({ "text": lines.join("\n"), "shots": per_shot, "scene_groups": groups, "strength": limits.name, "applied": apply, "undo_steps": undo_steps, "revision": ctx.shared.revision(), "look_from_brief": briefed, "next": if apply { "check with color reads every shot again; match_scenes holds the shots to each other, which is what the eye sees at a cut; skin_balance for a face still off the band; a look (klipp_look) for style, after the grade." } else { "That is where every shot stands against the look and the first step each would take. Run it with apply true to grade them, as one undo step." } }))
        }

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
            handle(ctx, ControlCommand::Export { path: draft.to_string_lossy().to_string(), width: Some(w), height: Some(h), fps: None, crf: Some(30), bit_depth: None, target_lufs: None, true_peak_dbtp: None, burn_captions: None }, &mut None)?;
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
                let check = s.spawn(|| handle(ctx, ControlCommand::Check { color: true }, &mut None));
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

        ControlCommand::Narrate { text, at_secs, voice, speed, pause_secs, download_models, apply, path } => {
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
                download: download_models,
                path: path.map(std::path::PathBuf::from),
                dir: &dir,
            })?;
            let mut v = handle(ctx, ControlCommand::ImportMedia { path: out.to_string_lossy().to_string(), analyze: true }, &mut None)?;
            let media_id = imported_id(ctx, &out).ok_or("the narration was made but did not reach the media pool")?;
            v["media_id"] = json!(media_id);
            v["path"] = json!(out);
            v["synth_secs"] = json!(t.elapsed().as_secs_f64());
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

        ControlCommand::Record { secs, target, fps, path, import } => {
            if in_batch {
                return Err("record takes the time it records; send it on its own".into());
            }
            if !secs.is_finite() || !(0.5..=600.0).contains(&secs) {
                return Err("secs is how long to record, half a second to ten minutes".into());
            }
            let out = match path {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    let dir = klipp_media::made::made_dir("recordings");
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    dir.join(format!("klipp-recording-{}.mp4", chrono_stamp()))
                }
            };
            let target = target.unwrap_or_else(|| "focused".into());
            let fps = fps.unwrap_or(60).clamp(1, 240);
            let child = if let Some(gsr) = which("gpu-screen-recorder") {
                let mut c = std::process::Command::new(gsr);
                if target.contains('+') && target.contains('x') {
                    c.args(["-w", "region", "-region", &target]);
                } else {
                    c.args(["-w", &target]);
                }
                c.args(["-f", &fps.to_string(), "-k", "h264", "-q", "high", "-cursor", "yes", "-o"]).arg(&out);
                c
            } else if let Some(wfr) = which("wf-recorder") {
                let mut c = std::process::Command::new(wfr);
                if target.contains('+') && target.contains('x') {
                    // wf-recorder wants "X,Y WxH".
                    let (size, pos) = target.split_once('+').unwrap_or((&target, "0+0"));
                    c.args(["-g", &format!("{} {}", pos.replace('+', ","), size)]);
                }
                c.args(["-r", &fps.to_string(), "-f"]).arg(&out);
                c
            } else {
                return Err("no screen recorder found: install gpu-screen-recorder (or wf-recorder) and it is used".into());
            }
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
            .spawn().map_err(|e| format!("could not start the recorder: {e}"))?;
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
            #[cfg(unix)]
            // SAFETY: SIGINT to a child this process spawned; the recorder finishes its file on it.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT); }
            let done = child.wait_with_output().map_err(|e| format!("the recorder: {e}"))?;
            if !out.is_file() {
                return Err(format!("the recorder wrote nothing ({}): {}", done.status, String::from_utf8_lossy(&done.stderr).lines().last().unwrap_or("")));
            }
            let mut v = json!({ "path": out, "secs": secs, "target": target });
            if import.unwrap_or(true) {
                handle(ctx, ControlCommand::ImportMedia { path: out.to_string_lossy().to_string(), analyze: true }, &mut None)?;
                v["media_id"] = json!(imported_id(ctx, &out));
                v["next"] = json!("Recorded and imported; analyze reads it (the beat sheet finds the reveals), then place or plan with it.");
            } else {
                v["next"] = json!("Recorded. import_media brings it into the pool.");
            }
            v["text"] = json!(format!("Recorded {secs:.1} s of {target} to {}", out.display()));
            Ok(v)
        }

        ControlCommand::AnalysisQueue { media_id, cancel_pending } => {
            if in_batch { return read_in_batch("analysis_queue"); }
            let id = media_id.as_deref().map(|s| parse_uuid(s, "media_id").map(klipp_core::MediaId)).transpose()?;
            let cancelled = klipp_analysis::service::manage_queue(id, cancel_pending)?;
            Ok(json!({ "cancelled": cancelled, "items": klipp_analysis::service::all() }))
        }

        ControlCommand::AnalysisStatus { wait, media_id, timeout_ms } => {
            if in_batch {
                return read_in_batch("analysis_status");
            }
            let ids = match media_id {
                Some(id) => vec![klipp_core::MediaId(parse_uuid(&id, "media_id")?)],
                None => klipp_analysis::service::all().iter().map(|s| parse_uuid(&s.media_id, "media_id").map(klipp_core::MediaId)).collect::<Result<_, _>>()?,
            };
            let mut reply = analysis_reply(&ids, wait, timeout_ms)?;
            reply["models"] = json!({
                "speech": klipp_analysis::transcript::installed_model().map(|(id, _)| id),
                "detection": klipp_analysis::framing::model_path().map(|(id, _)| id),
            });
            reply["note"] = json!("current is the analyser at work and current_for_secs how long it has been; a transcript takes about a third of the file's length");
            Ok(reply)
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
            // The whole of it, then cut: cut twice, the note gave the
            // length of the first cut as the length of the whole.
            let full = format!("{}  {:.2} s\n{}", item.filename, item.duration.to_secs_f64(), report.text(usize::MAX));
            let (text, cut) = cut_to_budget_with(&full, max_chars.unwrap_or(8000), "ask again with a larger max_chars, or read a stretch of the speech with transcript (from_secs, to_secs)");
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
                "beatsheet": report.beatsheet,
            }))
        }
        ControlCommand::BeatSheet { media_id } => {
            if in_batch {
                return read_in_batch("beat_sheet");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let status = klipp_analysis::service::status(id);
            match report.beatsheet {
                Some(bs) => {
                    let mut text = format!("{}  {:.2} s\n{}", item.filename, item.duration.to_secs_f64(), bs.summary());
                    // With the screen read, every beat says what it showed.
                    let labels = report.screentext.as_ref().map(|st| klipp_analysis::screentext::label_beats(&bs, st));
                    if let Some(l) = &labels {
                        text.push_str(&klipp_analysis::screentext::labelled_beats_text(&bs, l));
                    }
                    let beats: Vec<serde_json::Value> = bs.segments.iter().enumerate().map(|(i, s)| json!({
                        "start_secs": s.start_secs, "end_secs": s.end_secs, "kind": s.kind,
                        "label": labels.as_ref().and_then(|l| l.segments.get(i).cloned().flatten()),
                    })).collect();
                    let reveals: Vec<serde_json::Value> = bs.reveals.iter().enumerate().map(|(i, r)| json!({
                        "secs": r, "label": labels.as_ref().and_then(|l| l.reveals.get(i).cloned().flatten()),
                    })).collect();
                    Ok(json!({
                        "text": text,
                        "state": status.as_ref().map(|s| &s.state),
                        "cut_points": bs.cut_points,
                        "title_windows": bs.title_windows,
                        "reveals": bs.reveals,
                        "spots": bs.spots,
                        "segments": bs.segments,
                        "beats": beats,
                        "reveals_labelled": reveals,
                        "labelled": labels.is_some(),
                        "next": if labels.is_some() { "Plan by name: each settled beat says what the screen showed. Cut to its reveal and hold until it reads; confirm with frame_at where it matters." } else { "Cut to a strong cut point and hold on a settled stretch until it reads; look at the frame there (frame_at/contact_sheet) to confirm what is shown and refine the exact cut. With the OCR models downloaded, analyze names every beat by its on-screen text." },
                    }))
                }
                None => {
                    use klipp_analysis::service::State;
                    let message = match status.as_ref() {
                        Some(st @ klipp_analysis::service::Status { state: State::Running, .. }) => {
                            format!("Analysis is running: {} ({} s on this pass; {} passes complete). Wait with analysis_status, this media_id and wait true, then read beat_sheet again.", st.current.unwrap_or("starting the next pass"), st.current_for_secs.unwrap_or(0), st.done.len())
                        }
                        Some(klipp_analysis::service::Status { state: State::Queued, .. }) => "Analysis is queued. Wait with analysis_status, this media_id and wait true; analysis_queue can prioritize it.".into(),
                        Some(klipp_analysis::service::Status { state: State::Failed, .. }) => "Analysis failed; see analysis.failed for the pass errors. Retry analyze after fixing the reported cause.".into(),
                        Some(klipp_analysis::service::Status { state: State::Cancelled, .. }) => "Analysis was cancelled. Call analyze with this media_id to request it again.".into(),
                        Some(klipp_analysis::service::Status { state: State::Done, .. }) => "Analysis finished without a beat sheet. Check that the shots and motion passes are enabled and support this media.".into(),
                        None => "Run analyze first (the beat sheet needs the shots and motion passes), then read it again.".into(),
                    };
                    Ok(json!({
                        "text": format!("{}: no beat sheet yet. {message}", item.filename),
                        "state": status.as_ref().map(|s| &s.state),
                        "analysis": status,
                    }))
                },
            }
        }

        ControlCommand::FindShots { framing, min_people, object, motion, with_speech, min_secs, max_hits } => {
            if in_batch {
                return read_in_batch("find_shots");
            }
            // A search over files still being analysed is a search over
            // part of the footage: said, with which files.
            let pending: Vec<String> = klipp_analysis::service::all()
                .into_iter()
                .filter(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running))
                .map(|s| s.filename)
                .collect();
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
            let mut v = json!({ "hits": hits });
            if !pending.is_empty() {
                v["still_analysing"] = json!(pending);
                v["note"] = json!(format!("these files are still being analysed and are not in the hits yet: {}; see analysis_status and ask again when they are done", pending.join(", ")));
            }
            Ok(v)
        }

        ControlCommand::ContactSheet { media_id, from_secs, to_secs, frames, cols, tile_width, path, inline } => {
            let inline = inline.unwrap_or(path.is_none());
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
            let mut v = serde_json::to_value(sheet).map_err(|e| e.to_string())?;
            v["inline"] = json!(inline);
            Ok(v)
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
            let full = timeline_text(&seq, &pool, presented, ctx.shared.revision(), view);
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

        ControlCommand::ImportMedia { path, analyze } => {
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

        ControlCommand::NewProject { name, location, width, height, fps, sequence_name, discard_changes } => {
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
            send_envelope(ctx, vec![AppCommand::CreateProject { name, location, fps_num, fps_den, width, height, sequence_name }], false)?;
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
            let made = if width == 0 { "Empty project made; its size and rate follow the first clip placed." } else { "Empty project made." };
            Ok(json!({
                "created": true,
                "saved": saved,
                "path": dir.to_string_lossy(),
                "text": if saved { made.to_string() } else { format!("{made} Its first save has not landed yet: wait before reading the folder.") },
            }))
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

        ControlCommand::RenameSequence { name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err("a sequence needs a name".into());
            }
            send(AppCommand::RenameSequence { sequence_idx: usize::MAX, new_name: name })
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

        ControlCommand::TrackSurface { clip_id, corners, click, at_secs, name, surface, direction } => {
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
            let mut v = match (corners, click) {
                (Some(corners), _) => send(AppCommand::TrackSurface { clip_id: id, corners, at_secs, name, surface, direction, interactive: false })?,
                (None, Some(click)) => send(AppCommand::SnapSurface { clip_id: id, click, at_secs, then_track: true, name })?,
                (None, None) => unreachable!("refused above"),
            };
            v["text"] = json!(if click.is_some() {
                "Finding the surface's edges round the click, then tracking it, in the background: it shows under the clip's surfaces in klipp_list_clips when done. If nothing shows, there was no surface with an edge all round there: look at the frame and give its corners."
            } else {
                "Tracking the surface in the background; it shows under the clip's surfaces in klipp_list_clips when done, one undo step, with where the track was lost if it was. Then pin a picture to it with the pin op."
            });
            Ok(v)
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
            // The timeline grade runs on every frame after the clip's own,
            // so what the viewer shows is both; say so, or a reader of this
            // graph alone would misjudge the picture.
            let timeline = seq.timeline_graph.as_ref().filter(|g| !g.is_identity());
            // Likewise its grade group's grades, before and after its own (#237).
            let group = clip.grade_group.and_then(|g| seq.grade_group(g)).filter(|g| g.grades());
            Ok(json!({
                "clip_id": c,
                "is_identity": clip.own_grade().is_identity(),
                "graph": serde_json::to_value(clip.own_grade())
                    .map_err(|e| format!("serialising graph: {e}"))?,
                "grade_group": group.map(|g| json!({
                    "name": g.name,
                    "pre": g.pre.as_ref().filter(|p| !p.is_identity()),
                    "post": g.post.as_ref().filter(|p| !p.is_identity()),
                })),
                "timeline_graph": timeline
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|e| format!("serialising the timeline graph: {e}"))?,
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

        ControlCommand::Plan { ops, detail } => {
            if in_batch {
                return read_in_batch("plan");
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
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
            let (plan, revision) = resolve_plan(ctx, &ops)?;
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
            let mut v = send_full_envelope_for_project(ctx, Vec::new(), edits, true, Some(revision), expected_project_identity, Some(label))?;
            if v["applied"] == true {
                if let Some(changes) = cut_changes {
                    v["changes"] = changes["changes"].clone();
                    v["shifted"] = changes["shifted"].clone();
                }
            }
            v["text"] = json!(if detail { plan.text() } else { plan.text_folded() });
            with_warnings(&mut v, op_font_warnings(&ops));
            if detail {
                v["steps"] = json!(plan.report());
            }
            v["duration_secs"] = json!(plan.duration_secs);
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

        ControlCommand::Transcript { media_id, from_secs, to_secs, words } => {
            if in_batch {
                return read_in_batch("transcript");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(t) = report.transcript else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript yet (analysis: {state}); analyze it, and see analysis_status", item.filename));
            };
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(f64::MAX));
            let silences: Vec<(f64, f64)> = report.audio.map(|au| au.silences.iter().map(|s| (s.start_secs, s.end_secs)).filter(|(x, y)| *y > a && *x < b).collect()).unwrap_or_default();
            let mut out = format!("{}  speech {} ({}), times in seconds of the file\n", item.filename, t.language.as_deref().unwrap_or("?"), t.model);
            for seg in t.segments.iter().filter(|s| s.end_secs > a && s.start_secs < b) {
                out.push_str(&format!("[{:.2}-{:.2}] {}\n", seg.start_secs, seg.end_secs, seg.text));
                if words {
                    let line: Vec<String> = t.words.iter().filter(|w| w.start_secs >= seg.start_secs - 0.01 && w.start_secs < seg.end_secs && w.end_secs > a && w.start_secs < b).map(|w| format!("{}@{:.2}-{:.2}", w.text, w.start_secs, w.end_secs)).collect();
                    out.push_str(&format!("    {}\n", line.join(" ")));
                }
            }
            if !silences.is_empty() {
                let line: Vec<String> = silences.iter().map(|(x, y)| format!("{x:.2}-{y:.2}")).collect();
                out.push_str(&format!("Measured silence (a cut there is clean): {}\n", line.join(", ")));
            }
            let (text, cut) = cut_to_budget_with(&out, 12_000, "ask for a shorter stretch with from_secs and to_secs");
            Ok(json!({ "text": text, "cut": cut }))
        }

        ControlCommand::AlignAudio { clip_id: c, reference_clip_id: r, max_offset_secs } => {
            if in_batch {
                return Err("align_audio moves a clip; send it on its own, not in a batch".into());
            }
            let tgt_id = clip_id(&c)?;
            let ref_id = clip_id(&r)?;
            // The two media paths and source-in points, and the reference's
            // place on the timeline — read, then let the lock go for ffmpeg.
            let (tgt_path, tgt_src_in, ref_path, ref_src_in, ref_tl_in, tgt_name) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let find = |id: klipp_core::ClipId| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(move |cl| cl.id == id).cloned();
                let tgt = find(tgt_id).ok_or_else(|| format!("no clip with id {c}"))?;
                let refc = find(ref_id).ok_or_else(|| format!("no clip with id {r}"))?;
                let media_of = |cl: &klipp_timeline::Clip, which: &str| -> Result<std::path::PathBuf, String> {
                    let mid = cl.media_id().ok_or_else(|| format!("the {which} clip has no media"))?;
                    let item = pool.get(&mid).ok_or_else(|| format!("the {which} clip's media is gone"))?;
                    if !item.has_audio {
                        return Err(format!("the {which} clip ({}) has no sound to sync", item.filename));
                    }
                    Ok(item.path.clone())
                };
                let tp = media_of(&tgt, "target")?;
                let rp = media_of(&refc, "reference")?;
                let tname = pool.get(&tgt.media_id().unwrap_or(klipp_core::MediaId(uuid::Uuid::nil()))).map(|m| m.filename.clone()).unwrap_or_else(|| "the clip".into());
                (tp, tgt.source_in.to_secs_f64(), rp, refc.source_in.to_secs_f64(), refc.timeline_in.to_secs_f64(), tname)
            };
            const SR: u32 = 8_000;
            let max_off = max_offset_secs.unwrap_or(30.0).clamp(1.0, 300.0);
            // Decode enough past the search window to hold a real match.
            let window = max_off + 90.0;
            let ffmpeg = std::path::Path::new("ffmpeg");
            let ref_pcm = klipp_analysis::audio_sync::decode_mono_pcm(ffmpeg, &ref_path, SR, ref_src_in, window)?;
            let tgt_pcm = klipp_analysis::audio_sync::decode_mono_pcm(ffmpeg, &tgt_path, SR, tgt_src_in, window)?;
            let (off_samples, confidence) = klipp_analysis::audio_sync::find_sync_offset_scored(&ref_pcm, &tgt_pcm, SR, max_off as f32);
            let offset_secs = off_samples as f64 / SR as f64;
            if confidence < 0.2 {
                return Err(format!("{tgt_name} and the reference do not seem to share any sound (match {:.0}%); nothing moved. Use a reference clip recorded at the same moment.", confidence * 100.0));
            }
            let new_in = ref_tl_in - offset_secs;
            if new_in < 0.0 {
                return Err(format!("the offset {offset_secs:+.3} s would push {tgt_name} before the timeline start; move the reference clip later first, then align"));
            }
            use klipp_document::ops::{Anchor, ClipRef, Op};
            let op = Op::Move { clip_id: ClipRef::Id(tgt_id), to: Anchor::Secs(new_in), track: None };
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(format!("sync {tgt_name}")))?;
            v["text"] = json!(format!("Aligned {tgt_name} to the reference: offset {offset_secs:+.3} s, match {:.0}%. Moved it to {new_in:.3} s on the timeline.", confidence * 100.0));
            v["offset_secs"] = json!(offset_secs);
            v["confidence"] = json!(confidence);
            v["next"] = json!("Play across the join to confirm the sound and picture line up. If it drifts over a long take, the two clocks differ — you may need a second sync point.");
            Ok(v)
        }

        ControlCommand::Fonts => {
            if in_batch {
                return read_in_batch("fonts");
            }
            let bundled: Vec<String> = klipp_title::bundled::families().into_iter().map(|(name, _)| name.to_owned()).collect();
            let installed: Vec<String> = klipp_title::font_discovery::discover_system_fonts().keys().cloned().collect();
            let text = format!(
                "Bundled with Klipp, always there: {}.\nInstalled on this machine ({}): {}.\nA title or type style takes any of these by its family name; anything else falls back to {}.",
                bundled.join(", "), installed.len(), installed.join(", "), klipp_title::bundled::DEFAULT_FAMILY,
            );
            Ok(json!({ "text": text, "bundled": bundled, "installed": installed, "fallback": klipp_title::bundled::DEFAULT_FAMILY }))
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
            let knobs = match_grade_knobs(&target_stats, &ref_stats);
            if knobs.is_empty() {
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
            let (data, w, h) = grab_scope(ctx, m, secs, timeout_ms)?;
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
                "text": format!("{name} scope of the frame at {secs:.2} s, {w}x{h}, written to {}", out.display()),
                "next": "The scope is the colourist's second eye; frame_stats gives the same frame as numbers to grade to.",
            }))
        }

        ControlCommand::Selects { media_id, min_gap_secs } => {
            if in_batch {
                return read_in_batch("selects");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let Some(t) = report.transcript.as_ref() else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript yet (analysis: {state}); analyze it first", item.filename));
            };
            let words: Vec<(f64, f64, String)> = t.words.iter().map(|w| (w.start_secs, w.end_secs, w.text.clone())).collect();
            if words.is_empty() {
                return Err(format!("{} has no speech to cut down", item.filename));
            }
            let gap = min_gap_secs.unwrap_or(0.6).max(0.1);
            let selects = suggest_selects(&words, gap, 0.35);
            let keep: Vec<&Select> = selects.iter().filter(|s| !s.filler).collect();
            let kept_secs: f64 = keep.iter().map(|s| s.out_secs - s.in_secs).sum();
            let dur = item.duration.to_secs_f64();
            let mut text = format!("{}: {} selects to keep out of {} runs, {:.1} s of {:.1} s ({:.0}%). Place each keep range, in order, closing the gaps.\n", item.filename, keep.len(), selects.len(), kept_secs, dur, if dur > 0.0 { 100.0 * kept_secs / dur } else { 0.0 });
            for s in &selects {
                let mark = if s.filler { "drop" } else { "keep" };
                let mut line = s.text.clone();
                if line.chars().count() > 80 { line = line.chars().take(79).collect::<String>() + "…"; }
                text.push_str(&format!("  [{mark}] {:.2}-{:.2} s  \"{}\"\n", s.in_secs, s.out_secs, line));
            }
            let (text, cut) = cut_to_budget_with(&text, 12_000, "narrow with a larger min_gap_secs");
            Ok(json!({
                "text": text, "cut": cut, "media_id": media_id, "filename": item.filename,
                "selects": selects.iter().map(|s| json!({ "in_secs": s.in_secs, "out_secs": s.out_secs, "text": s.text, "keep": !s.filler })).collect::<Vec<_>>(),
                "kept_secs": kept_secs, "duration_secs": dur,
                "next": "Place each keep range (place with the clip's source in/out at these times), in order, on the timeline; drop the rest. Then cover any jump cuts and check.",
            }))
        }

        ControlCommand::SearchMusic { query, limit, commercial_only, instrumental, listen } => {
            let listen = listen.unwrap_or(Listen::First(0));
            if in_batch {
                return read_in_batch("search_music");
            }
            if !klipp_audio_search::music_available() {
                return Err("music search is not set up: put jamendo_client_id in ~/.config/klipp/keys.toml (or set KLIPP_JAMENDO_CLIENT_ID)".into());
            }
            let tracks = klipp_audio_search::search_music_filtered(&query, limit.unwrap_or(12), klipp_audio_search::MusicFilter { commercial_only, instrumental })?;
            // The previews asked for heard, side by side, so a candidate is
            // judged before it is imported (#739): the first few, or those
            // named by id (#795).
            let heard = hear_previews(tracks.iter().enumerate().map(|(i, t)| listen.picks(i, &t.id).then(|| t.preview_url.clone())).collect(), listen_to_preview);
            let mut text = format!("{} royalty-free music results for \"{query}\" (Jamendo), best match to the words first:\n", tracks.len());
            for (t, h) in tracks.iter().zip(&heard) {
                let pace = if t.pace.is_empty() { String::new() } else { format!(", {} pace", t.pace) };
                text.push_str(&format!("  {} — {}  ({}, {:.0}s{pace}){}  [{}]\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }, t.tags.join(", ")));
                match h {
                    Some(Ok(l)) => text.push_str(&format!("      heard: {}\n", l.describe())),
                    Some(Err(e)) => text.push_str(&format!("      not heard: {e}\n")),
                    None => {}
                }
            }
            let results: Vec<serde_json::Value> = tracks.iter().zip(&heard).map(|(t, h)| {
                let mut v = json!({
                    "title": t.title, "artist": t.artist, "track_id": t.id,
                    "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                    "commercial_ok": t.commercial_ok, "preview_url": t.preview_url, "download_url": t.download_url,
                    "page_url": t.page_url, "tags": t.tags, "pace": t.pace,
                });
                if let Some(Ok(l)) = h {
                    v["heard"] = json!({ "bpm": l.bpm, "beat_confidence": l.beat_confidence, "loudness_db": l.loudness_db, "energy_db": l.energy_db, "shape": l.shape });
                }
                v
            }).collect();
            let next = if listen.is_none() {
                "Judge candidates before importing: search again with listen: 3 to hear the first three, or listen: [track_id, …] for those you name (tempo, beat, energy over the track). Add one with add_music (copy its title, artist, download_url, license, license_url, page_url). Honour the licence: non-commercial tracks stay out of commercial work, and the credit is kept for you."
            } else {
                "Pick by the brief's energy: a steady or gently building track sits under a calm piece, a driving one under a fast montage. Add one with add_music (copy its title, artist, download_url, license, license_url, page_url). Honour the licence: non-commercial tracks stay out of commercial work, and the credit is kept for you."
            };
            Ok(json!({ "text": text, "results": results, "next": next }))
        }

        ControlCommand::SearchSfx { query, limit, max_secs, listen } => {
            if in_batch {
                return read_in_batch("search_sfx");
            }
            if !klipp_audio_search::sfx_available() {
                return Err("sound-effect search is not set up: put freesound_token in ~/.config/klipp/keys.toml (or set KLIPP_FREESOUND_TOKEN)".into());
            }
            if max_secs.is_some_and(|m| !(m.is_finite() && m > 0.0)) {
                return Err("max_secs is a length in seconds, above zero".into());
            }
            let listen = listen.unwrap_or(Listen::First(0));
            let tracks = klipp_audio_search::search_sfx_within(&query, limit.unwrap_or(12), max_secs)?;
            let heard = hear_previews(tracks.iter().enumerate().map(|(i, t)| listen.picks(i, &t.id).then(|| t.preview_url.clone())).collect(), listen_to_sfx);
            let within = max_secs.map(|m| format!(", {m:.1} s or shorter")).unwrap_or_default();
            let mut text = format!("{} royalty-free sound effects for \"{query}\" (Freesound){within}:\n", tracks.len());
            for (t, h) in tracks.iter().zip(&heard) {
                text.push_str(&format!("  {} — {}  ({}, {:.1}s){}  id {}\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }, t.id));
                match h {
                    Some(Ok(l)) => text.push_str(&format!("      heard: {}\n", l.describe())),
                    Some(Err(e)) => text.push_str(&format!("      not heard: {e}\n")),
                    None => {}
                }
            }
            let results: Vec<serde_json::Value> = tracks.iter().zip(&heard).map(|(t, h)| {
                let mut v = json!({
                    "title": t.title, "artist": t.artist, "track_id": t.id,
                    "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                    "commercial_ok": t.commercial_ok, "preview_url": t.preview_url, "download_url": t.download_url,
                    "page_url": t.page_url, "tags": t.tags,
                });
                if let Some(Ok(l)) = h {
                    v["heard"] = json!({ "secs": l.secs, "peak_secs": l.peak_secs, "loudness_db": l.loudness_db, "peak_db": l.peak_db });
                }
                v
            }).collect();
            Ok(json!({ "text": text, "results": results, "next": "Hear candidates with listen (a count, or [track_id, …]): a sound's length and when it peaks say where it goes, the peak on the cut. Add one with add_music (it takes a sound effect just the same: pass title, artist, preview_url, license, license_url, page_url). The credit is kept; honour the licence." }))
        }

        ControlCommand::AddMusic { title, artist, track_id, download_url, preview_url, license, license_url, page_url } => {
            if in_batch {
                return Err("add_music downloads and imports a file; send it on its own, not in a batch".into());
            }
            let track = klipp_audio_search::Track {
                source: klipp_audio_search::Source::JamendoMusic,
                id: track_id, title: title.clone(), artist: artist.clone(),
                duration_secs: 0.0, license: license.clone(), license_url,
                commercial_ok: true, preview_url, download_url, page_url, image_url: String::new(), tags: Vec::new(), pace: String::new(),
            };
            let path = klipp_audio_search::download_track(&track, &klipp_audio_search::download_dir())?;
            let mut v = handle(ctx, ControlCommand::ImportMedia { path: path.to_string_lossy().to_string(), analyze: true }, &mut None)?;
            v["text"] = json!(format!("Added \"{title}\" by {artist} ({license}) to the media pool. Place it on an audio track (place marks it music) and duck it under speech.", license = if license.is_empty() { "licence with the file".to_string() } else { license }));
            v["attribution"] = json!(track.attribution());
            v["next"] = json!("Place it on an audio track (place marks it music, or sfx for an effect) and duck it under any speech. The credit is saved beside the file for your export notes.");
            Ok(v)
        }

        ControlCommand::Beats { media_id } => {
            if in_batch {
                return read_in_batch("beats");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, has_audio) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.has_audio)
            };
            if !has_audio {
                return Err(format!("{filename} has no sound to find a beat in"));
            }
            let b = klipp_analysis::beats::detect(std::path::Path::new("ffmpeg"), &path, 0.0)?;
            let text = if b.confidence < 0.3 {
                format!("{filename}: no clear beat (match {:.0}%). Probably speech or ambience, not music.", b.confidence * 100.0)
            } else {
                format!("{filename}: {:.0} BPM, match {:.0}%, {} beats over the read. Cut or place on a beat time.", b.bpm, b.confidence * 100.0, b.beat_times.len())
            };
            // The grid, not the list: the first beat and the interval say
            // where every beat is, and a few are given to check the drift.
            let first = b.beat_times.first().copied().unwrap_or(0.0);
            let interval: f64 = if b.bpm > 0.0 { 60.0 / b.bpm as f64 } else { 0.0 };
            let first_beats: Vec<f64> = b.beat_times.iter().take(32).map(|t| (t * 1000.0).round() / 1000.0).collect();
            let last = b.beat_times.last().copied().unwrap_or(first);
            let drift = if b.beat_times.len() > 1 && interval > 0.0 { last - (first + interval * (b.beat_times.len() - 1) as f64) } else { 0.0 };
            let text = format!("{text}\nFirst beat at {first:.3} s, then every {interval:.4} s ({:.1} s a bar of four); beat n = {first:.3} + n × {interval:.4}, drifting {drift:+.2} s by the end of the read. The first {} beats: {}", interval * 4.0, first_beats.len(), first_beats.iter().map(|t| format!("{t:.2}")).collect::<Vec<_>>().join(" "));
            Ok(json!({ "text": text, "media_id": media_id, "bpm": b.bpm, "confidence": b.confidence, "first_beat_secs": first, "beat_interval_secs": interval, "bar_secs": interval * 4.0, "beats_read": b.beat_times.len(), "drift_secs": drift, "first_beats": first_beats, "next": "Cut on beat n = first + n × interval; a shot of two bars is 8 beats. style_profile reads a whole reference at once." }))
        }

        ControlCommand::StyleProfile { media_id } => {
            if in_batch {
                return read_in_batch("style_profile");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let (path, filename, dur, has_audio) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
                (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64(), item.has_audio)
            };
            let report = klipp_analysis::report::MediaAnalysis::load(&path);
            let Some(shots) = report.shots.as_ref() else {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{filename} has no shot analysis yet (analysis: {state}); analyse the reference first, then read its style"));
            };
            let shot_pairs: Vec<(f64, f64)> = shots.shots.iter().map(|s| (s.start_secs, s.end_secs)).collect();
            let sdur = if shots.duration_secs > 0.0 { shots.duration_secs } else { dur };
            let beats = if has_audio {
                klipp_analysis::beats::detect(std::path::Path::new("ffmpeg"), &path, 0.0).unwrap_or(klipp_analysis::beats::BeatData { bpm: 0.0, confidence: 0.0, beat_times: Vec::new() })
            } else {
                klipp_analysis::beats::BeatData { bpm: 0.0, confidence: 0.0, beat_times: Vec::new() }
            };
            let cuts = style_cut_summary(&shot_pairs, &shots.boundaries, sdur, &beats);

            // Look: measure a spread of frames off the file through the scopes.
            let n = 8usize;
            let (mut black, mut mid, mut white, mut sat, mut warm, mut cnt) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            let mut measures: Vec<klipp_color::measure::FrameMeasure> = Vec::new();
            for i in 0..n {
                let t = (i as f64 + 0.5) / n as f64 * dur;
                if let Ok((data, w, h)) = decode_frame_rgba(&path, t) {
                    let faces: Vec<[f32; 4]> = report.framing.as_ref().and_then(|f| f.people_at(t, 1.5).map(|s| face_or_head_regions(f, s))).unwrap_or_default();
                    measures.push(klipp_color::measure::measure_frame(&data, w, h, &faces));
                    let st = frame_stats(&data, w, h, t);
                    let g = |a: &str, b: &str| st.get(a).and_then(|x| x.get(b)).and_then(|x| x.as_f64()).unwrap_or(0.0);
                    black += g("luma_ire", "black");
                    mid += g("luma_ire", "mid");
                    white += g("luma_ire", "white");
                    sat += st.get("saturation_pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    warm += g("rgb_balance", "red") - g("rgb_balance", "blue");
                    cnt += 1.0;
                }
            }
            let look = if cnt > 0.0 {
                let (black, mid, white, sat, warm) = (black / cnt, mid / cnt, white / cnt, sat / cnt, warm / cnt * 100.0);
                let contrast = white - black;
                let tone = if warm > 3.0 { "warm" } else if warm < -3.0 { "cool" } else { "neutral" };
                let punch = if contrast > 75.0 { "high-contrast" } else if contrast < 45.0 { "flat" } else { "medium-contrast" };
                let col = if sat > 45.0 { "saturated" } else if sat < 20.0 { "muted" } else { "natural" };
                json!({ "black_ire": black.round(), "mid_ire": mid.round(), "white_ire": white.round(), "contrast_ire": contrast.round(), "saturation_pct": sat.round(), "warmth": (warm.round()), "summary": format!("{tone}, {punch}, {col}"), "look_target": klipp_color::measure::LookTarget::from_measures(&format!("like {filename}"), &measures) })
            } else {
                json!(null)
            };
            let mut response = style_profile_response(&filename, &media_id, cuts, &beats, look,
                StylePacingInput { shots, motion: report.motion.as_ref(), sheet: report.beatsheet.as_ref() });
            if report.motion.as_ref().is_none_or(|m| m.overall_mean().is_none())
                && klipp_analysis::service::status(id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running)) {
                response["next"] = json!("Motion analysis is queued or running; wait for it to finish, then read style_profile again. Do not queue a second analysis.");
                for pointer in ["/text", "/guidance", "/pacing_evidence/guidance"] {
                    if let Some(value)=response.pointer_mut(pointer) {
                        if let Some(text)=value.as_str() {
                            *value=json!(text.replace("Re-analyse the reference and check that its video stream decodes.", "Motion analysis is still in progress; wait for completion.")
                                .replace("Re-analyse the reference to populate missing or outdated analysis.", "Motion analysis is still in progress; wait for completion."));
                        }
                    }
                }
            }
            Ok(response)
        }

        ControlCommand::CutPoints { media_id, near_secs, from_secs, to_secs } => {
            if in_batch {
                return read_in_batch("cut_points");
            }
            let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
            let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
            let dur = item.duration.to_secs_f64();
            let (a, b) = (from_secs.unwrap_or(0.0), to_secs.unwrap_or(dur));
            let words: Vec<(f64, f64, String)> = report.transcript.as_ref().map(|t| t.words.iter().map(|w| (w.start_secs, w.end_secs, w.text.clone())).collect()).unwrap_or_default();
            let segments: Vec<(f64, f64, String)> = report.transcript.as_ref().map(|t| t.segments.iter().map(|s| (s.start_secs, s.end_secs, s.text.clone())).collect()).unwrap_or_default();
            let silences: Vec<(f64, f64)> = report.audio.as_ref().map(|au| au.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default();
            let shots: Vec<f64> = report.shots.as_ref().map(|s| s.boundaries.clone()).unwrap_or_default();
            if words.is_empty() && segments.is_empty() && silences.is_empty() && shots.is_empty() {
                let state = klipp_analysis::service::status(id).map(|s| format!("{:?}", s.state).to_lowercase()).unwrap_or_else(|| "not asked for".into());
                return Err(format!("{} has no transcript, silence or shot analysis yet (analysis: {state}); analyze it, and see analysis_status", item.filename));
            }
            let mut v = cut_points(&words, &segments, &silences, &shots, near_secs, (a, b));
            v["media_id"] = json!(media_id);
            v["filename"] = json!(item.filename);
            v["next"] = json!("Cut on a strength-3 pause where you can; a strength-2 full stop or shot change is clean too. Trim or place to the point's secs. Then check.");
            Ok(v)
        }

        ControlCommand::PrepareCut { from_secs, to_secs, near_secs, intent, max_candidates } => {
            if in_batch { return read_in_batch("prepare_cut"); }
            cut_prep::prepare(ctx, from_secs, to_secs, near_secs, intent, max_candidates)
        }

        ControlCommand::Orient => {
            if in_batch {
                return read_in_batch("orient");
            }
            let project = project_status(ctx)?;
            let (brief, edit_plan) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                (seq.brief.clone(), seq.edit_plan.clone())
            };
            let statuses = klipp_analysis::service::all();
            let state_of = |path: &std::path::Path| -> serde_json::Value {
                let filename = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                match statuses.iter().find(|s| s.filename == filename) {
                    Some(s) => json!({ "state": format!("{:?}", s.state).to_lowercase(), "done": s.done, "current": s.current, "current_for_secs": s.current_for_secs }),
                    None => json!({ "state": "not analysed" }),
                }
            };
            let (media, all_analysed, any_running) = {
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let media: Vec<serde_json::Value> = pool.all().map(|m| json!({
                    "media_id": m.id.0.to_string(),
                    "filename": m.filename,
                    "duration_secs": m.duration.to_secs_f64(),
                    "width": m.resolution.width,
                    "height": m.resolution.height,
                    "fps": m.fps.as_f64(),
                    "has_audio": m.has_audio,
                    "analysis": state_of(&m.path),
                })).collect();
                let all_analysed = !media.is_empty() && statuses.iter().filter(|s| pool.all().any(|m| m.filename == s.filename)).all(|s| matches!(s.state, klipp_analysis::service::State::Done));
                let any_running = statuses.iter().any(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running));
                (media, all_analysed, any_running)
            };
            let (seq_text, clips) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
                (timeline_text(&seq, &pool, None, ctx.shared.revision(), TimelineView::default()), clips)
            };
            // What to do next, from where the work stands.
            let next = if media.is_empty() {
                "Import the footage with import_media. Then set the brief with set_brief (its title, length, platform, loudness, captions, and the cast with names and spellings), and analyze."
            } else if brief.is_none() {
                "Set the brief with set_brief: the piece's title, target length, platform, loudness, whether it is captioned, and the cast with their names and how things are spelt. The spellings sharpen the transcript."
            } else if any_running {
                "Analysis is still running (see the media states). Read the transcript and the shots for the files that are done, and pull selects, while the rest finishes."
            } else if !all_analysed {
                "Analyze the footage: analyze runs shots, sound, motion, the transcript and framing. Then read a transcript to pull selects, and find_shots for B-roll."
            } else if clips == 0 && edit_plan.is_none() {
                "The footage is analysed. Propose the paper edit: read each file's beat_sheet (and transcript, where there is speech) and set_edit_plan with one segment per beat of the brief's arc — a stretch of a file for each ranked feature, a note saying what it shows, titles where the beat sheet has a hold window. Then show it to the user for approval."
            } else if clips == 0 && edit_plan.as_ref().is_some_and(|p| !p.approved) {
                "An edit plan is proposed and awaits approval. Show it to the user (edit_plan gives the text); change it with set_edit_plan if they ask; on their word, approve_edit_plan, then build_edit_plan."
            } else if clips == 0 {
                "The edit plan is approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step."
            } else {
                "Clips are down. Look with frame_at and contact_sheet, run check to hold the cut to the brief and the craft, fix what it finds, then export at the brief's loudness and save_project."
            };
            Ok(json!({
                "build": build_json(),
                "project": {
                    "path": project.path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "unsaved_changes": project.dirty,
                    "unsaved_changes_from": if project.dirty { project.changed_by.clone().or(Some("edits".into())) } else { None },
                },
                "brief": brief.as_ref().map(|b| b.text()),
                "edit_plan": edit_plan.as_ref().map(|p| p.text(brief.as_ref().and_then(|b| b.target_secs))),
                "media": media,
                "sequence": seq_text,
                "clips": clips,
                "revision": ctx.shared.revision(),
                "next": next,
                "guide": "Call klipp_guide for how to work here, the craft rules and worked recipes.",
            }))
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

        ControlCommand::EditPlan => {
            if in_batch {
                return read_in_batch("edit_plan");
            }
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            Ok(match &seq.edit_plan {
                Some(p) => {
                    let ops = klipp_document::edit_plan::to_ops(p, &seq, &pool, seq.brief.as_ref());
                    json!({
                        "plan": p,
                        "text": p.text(seq.brief.as_ref().and_then(|b| b.target_secs)),
                        "approved": p.approved,
                        "ops": ops,
                        "next": if p.approved { "Approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step." } else { "Show the plan to the user and ask for their approval; on their word, approve_edit_plan, then build_edit_plan." },
                    })
                }
                None => json!({ "plan": null, "text": "There is no edit plan. Propose one with set_edit_plan: the segments in order, each a stretch of a file for a feature of the brief, with a note saying what it shows and why; titles, joins and a music bed as the piece wants them." }),
            })
        }

        ControlCommand::SetEditPlan { mut plan } => {
            if in_batch {
                return Err("set_edit_plan is its own undo step; send it on its own".into());
            }
            let (old, brief, text, snapped) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                klipp_document::edit_plan::validate(&plan, &pool, seq.brief.as_ref())?;
                // Cut on the bars: the bed's beats, read now, round every
                // segment to whole bars and start the bed on its first beat.
                let mut snapped: Vec<String> = Vec::new();
                if plan.snap_to_bars {
                    let Some(m) = &plan.music else { return Err("snap_to_bars needs music in the plan: the bars are the bed's".into()) };
                    let path = pool.get(&m.media_id).map(|it| it.path.clone()).ok_or("music: no such media item")?;
                    let beats = klipp_analysis::beats::detect(std::path::Path::new("ffmpeg"), &path, 0.0)?;
                    if beats.bpm <= 0.0 || beats.beat_times.is_empty() {
                        snapped.push("no beat was found in the music; nothing snapped".into());
                    } else {
                        let bar = 240.0 / beats.bpm as f64;
                        snapped = klipp_document::edit_plan::snap_to_bars(&mut plan, bar, beats.beat_times[0], &pool);
                        snapped.insert(0, format!("bars of {bar:.3} s at {:.1} bpm", beats.bpm));
                        klipp_document::edit_plan::validate(&plan, &pool, seq.brief.as_ref())?;
                    }
                }
                // A proposal is never its own approval.
                plan.approved = false;
                let text = plan.text(seq.brief.as_ref().and_then(|b| b.target_secs));
                (seq.edit_plan.clone(), seq.brief.clone(), text, snapped)
            };
            let missing: Vec<String> = brief.as_ref().map(|b| b.ranked_features().iter().filter(|f| !plan.segments.iter().any(|s| klipp_document::edit_plan::covers_feature(s, &f.title))).map(|f| f.title.clone()).collect()).unwrap_or_default();
            let edit = klipp_document::history::EditCommand::SetEditPlan { old: old.map(Box::new), new: Some(Box::new(plan)) };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Set Edit Plan".into()))?;
            v["text"] = json!(text);
            if !missing.is_empty() {
                v["note"] = json!(format!("the brief's features not in the plan: {}", missing.join(", ")));
            }
            if !snapped.is_empty() {
                v["snapped"] = json!(snapped);
            }
            v["next"] = json!("Show the plan to the user, as text, and ask whether to build it. On their word call approve_edit_plan; then build_edit_plan to see what it builds, and build_edit_plan with apply true to build it.");
            Ok(v)
        }

        ControlCommand::ApproveEditPlan => {
            if in_batch {
                return Err("approve_edit_plan is its own undo step; send it on its own".into());
            }
            let old = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.edit_plan.clone();
            let Some(old) = old else {
                return Err("there is no edit plan to approve; propose one with set_edit_plan".into());
            };
            if old.approved {
                return Ok(json!({ "approved": true, "revision": ctx.shared.revision(), "text": "The edit plan is already approved.", "next": "build_edit_plan shows what it builds; with apply true it builds it." }));
            }
            let mut new = old.clone();
            new.approved = true;
            let edit = klipp_document::history::EditCommand::SetEditPlan { old: Some(Box::new(old)), new: Some(Box::new(new)) };
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Approve Edit Plan".into()))?;
            v["approved"] = json!(true);
            v["next"] = json!("Approved. build_edit_plan shows what it builds; build_edit_plan with apply true builds it as one undo step. Then look (frame_at, contact_sheet), check, and export.");
            Ok(v)
        }

        ControlCommand::BuildEditPlan { apply, detail } => {
            if in_batch {
                return Err("build_edit_plan is its own batch; send it on its own".into());
            }
            let ops = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let Some(plan) = seq.edit_plan.as_ref() else {
                    return Err("there is no edit plan to build; propose one with set_edit_plan".into());
                };
                if !plan.approved {
                    return Err("the edit plan is not approved; show it to the user and, on their word, approve_edit_plan".into());
                }
                klipp_document::edit_plan::validate(plan, &pool, seq.brief.as_ref()).map_err(|e| format!("the edit plan no longer builds: {e}"))?;
                klipp_document::edit_plan::to_ops(plan, &seq, &pool, seq.brief.as_ref())
            };
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let mut v = if apply {
                let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Build Edit Plan".into()))?;
                v["next"] = json!("Built. Look at it (frame_at at each cut, contact_sheet), run check to hold it to the brief and the craft, fix what it finds, then export at the brief's loudness and save_project.");
                v
            } else {
                json!({ "revision": revision, "applied": false, "next": "That is what it builds. build_edit_plan with apply true builds it as one undo step." })
            };
            v["text"] = json!(if detail { plan.text() } else { plan.text_folded() });
            with_warnings(&mut v, op_font_warnings(&ops));
            v["steps_resolved"] = json!(plan.steps.len());
            v["duration_secs"] = json!(plan.duration_secs);
            v["ops"] = json!(ops);
            if detail {
                v["steps"] = json!(plan.report());
            }
            Ok(v)
        }

        ControlCommand::UpdateBrief { fields } => {
            if in_batch {
                return Err("update_brief is its own undo step; send it on its own".into());
            }
            let current = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone().unwrap_or_default();
            let brief = merge_brief(&current, &fields)?;
            handle(ctx, ControlCommand::SetBrief { brief }, &mut None)
        }

        ControlCommand::SetBrief { mut brief } => {
            if in_batch {
                return Err("set_brief is its own undo step; send it on its own".into());
            }
            for style in &brief.type_styles {
                klipp_document::title_style::resolve_motion(style)
                    .map_err(|e| format!("type style {:?}: {e}", style.name))?;
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
            if brief.features.iter().any(|f| f.title.trim().is_empty()) {
                return Err("a feature needs a title".into());
            }
            if let Some(dup) = brief.features.iter().enumerate().find(|(i, f)| brief.features[..*i].iter().any(|d| d.title.trim().eq_ignore_ascii_case(f.title.trim()))) {
                return Err(format!("the feature \"{}\" is listed twice", dup.1.title));
            }
            if let Some(look) = brief.look.take() {
                let name = look.name.clone();
                let resolved = resolve_look_target(look);
                if resolved.mid_l.is_none() && resolved.cast_a.is_none() && !resolved.screencast {
                    return Err(format!("look \"{name}\" is neither a preset ({}) nor numbers; give a name, or the fields black_l, white_l, mid_l, contrast_range, chroma_mean, cast_a, cast_b, skin_hue — style_profile lifts them from a reference", klipp_color::measure::LookTarget::PRESETS.join(", ")));
                }
                brief.look = Some(resolved);
            }
            if let Some(c) = brief.brand_colors.iter().find(|c| { let c = c.trim(); !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())) }) {
                return Err(format!("\"{c}\" is not a brand colour; give #rrggbb"));
            }
            let old = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone();
            let fonts_named: Vec<String> = brief.type_styles.iter().filter_map(|t| t.font.clone()).collect();
            let text = brief.text();
            let new_spellings = brief.spellings();
            let edit = klipp_document::history::EditCommand::SetBrief { old: old.clone().map(Box::new), new: Some(Box::new(brief)) };
            let respelt = old.as_ref().map(|b| b.spellings()).unwrap_or_default() != new_spellings;
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Set Brief".into()))?;
            v["text"] = json!(text);
            with_warnings(&mut v, font_warnings(fonts_named.iter().map(String::as_str)));
            if respelt && !new_spellings.is_empty() {
                // Only when a transcript was made without them: one that
                // already carries them is as good as it will get.
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let lacking: Vec<String> = pool
                    .all()
                    // Only what this project has analysed: a transcript in
                    // the cache from another day is re-made when the item
                    // is analysed here, and is nothing to warn about yet.
                    .filter(|m| klipp_analysis::service::status(m.id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Done)))
                    .filter_map(|m| klipp_analysis::report::MediaAnalysis::load(&m.path).transcript.map(|t| (m.filename.clone(), t.vocabulary)))
                    .filter(|(_, had)| klipp_analysis::transcript::lacks_spellings(had, &new_spellings))
                    .map(|(f, _)| f)
                    .collect();
                let analysed_any = pool.all().any(|m| klipp_analysis::service::status(m.id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Done)));
                if !lacking.is_empty() {
                    v["note"] = json!(format!("the spellings changed and these transcripts were made without them: {}; run analyze to have them transcribed again before captions or a transcript search", lacking.join(", ")));
                } else if analysed_any {
                    v["note"] = json!("the transcripts made so far already carry these spellings; a file analysed from now on gets them too");
                }
            }
            Ok(v)
        }

        ControlCommand::Check { color } => {
            if in_batch {
                return read_in_batch("check");
            }
            // The colour pass grabs frames, so it takes the sequence read
            // lock for a moment only; work out the times first, then let go.
            let (report, color_times, audio_media) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let report = klipp_document::qc::check(&seq, &pool, &AnalysedTranscripts);
                // Each detected shot of each picture clip, read on its own
                // — a clip holding a cut, or a long walk through changing
                // light, is several shots, and one median across them all
                // judged none of them (#329) — on every visible video
                // track: a short clip on a track above (an overlay, a
                // reveal) used to stand for the whole piece, and the main
                // picture under it went unread (#720). Each clip is read
                // alone, so what is over it is not taken for it (#719).
                // Up to six shots a clip (the longest), fewer when there
                // are many clips, 48 in all.
                let times: Vec<(f64, (f64, f64), klipp_core::ClipId)> = if color {
                    let clips: Vec<&klipp_timeline::Clip> = seq.tracks.iter()
                        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
                        .flat_map(|t| t.clips.iter())
                        .filter(|c| is_picture_clip(c))
                        .collect();
                    let per_clip = (48 / clips.len().max(1)).clamp(1, 6);
                    clips.into_iter()
                        .flat_map(|c| clip_shot_spans(c, &pool, seq.fps.as_f64(), per_clip).into_iter().map(move |span| (span, c.id)))
                        .map(|((a, b), id)| ((a + b) / 2.0, (a, b), id))
                        .take(48).collect()
                } else {
                    Vec::new()
                };
                // Each distinct piece of media that carries sound, with a
                // time it plays at — for reading its spectral faults off
                // the analysis (no rendering, so this is always on).
                // With it, the fixes every clip that plays the file's
                // sound already carries (#505).
                let mut audio_media: Vec<(std::path::PathBuf, String, f64, AudioFixes)> = Vec::new();
                for t in &seq.tracks {
                    for c in t.clips.iter().filter(|c| c.enabled) {
                        let Some(item) = c.media_id().and_then(|mid| pool.get(&mid)) else { continue };
                        if !item.has_audio {
                            continue;
                        }
                        // Heard: a clip on an audio track, or a picture clip
                        // playing its own sound. A picture whose sound is its
                        // linked audio clip is not where the sound is fixed.
                        let heard = t.kind == klipp_timeline::TrackKind::Audio || (c.linked_clip_id.is_none() && c.volume > 0.0);
                        // A sound linked to a picture keeps its settings — the
                        // cleaners among them — on the picture clip, so they
                        // are read there (#725).
                        let fixes = if heard { AudioFixes::on(&sound_owner(&seq, c).audio_effects) } else { AudioFixes::ALL };
                        match audio_media.iter_mut().find(|m| m.0 == item.path) {
                            Some(m) => m.3 = m.3.and(fixes),
                            None => audio_media.push((item.path.clone(), item.filename.clone(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0, fixes)),
                        }
                    }
                }
                (report, times, audio_media)
            };
            // Every shot read in Lab against the brief's look (or the
            // neutral one, or the screen recording's), faces and all.
            let mut color_findings: Vec<(String, f64, String, String)> = Vec::new();
            let mut shots_on_target = 0usize;
            // A survey of many shots reads each once, at its middle; a few
            // are read three times across, so drift within a shot is said.
            let survey = color_times.len() > 8;
            let mut unread: Vec<String> = Vec::new();
            for (t, span, clip) in &color_times {
                let read = if survey {
                    measure_at(ctx, *t, Some(*clip), Some(4000)).map(|m| (vec![(*t, m)], None))
                } else {
                    measure_clip(ctx, span.0, span.1, Some(*clip), Some(4000))
                };
                let (reads, drift) = match read {
                    Ok(r) => r,
                    Err(e) => {
                        unread.push(format!("{t:.2} s ({e})"));
                        continue;
                    }
                };
                let (_, m) = median_measure(&reads);
                let look = look_for(ctx, *t);
                let mut found = colour_report(&m, &look, *t);
                // The chain's own order, which no measurement can see:
                // a balance behind a look never settles, however many
                // rounds the loop runs.
                {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    if let Some(c) = seq.tracks.iter().flat_map(|tr| tr.clips.iter()).find(|c| c.id == *clip) {
                        for fault in c.own_grade().chain_out_of_order() {
                            found.push(("warning".to_string(), *t, format!("{fault} [grade order]"), "grade_chain shows the chain; the order is input, balance, secondary, look, output".to_string()));
                        }
                    }
                }
                if let Some(d) = drift {
                    found.push(("warning".to_string(), *t, format!("{d} [drift]"), "keyframe the grade across the shot, or cut where the light changes".to_string()));
                }
                if found.is_empty() {
                    shots_on_target += 1;
                }
                color_findings.extend(found);
            }
            let departing: Vec<String> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                seq.grade_groups.iter().filter(|g| g.departs && seq.all_clips().any(|c| c.grade_group == Some(g.id))).map(|g| g.name.clone()).collect()
            };
            // Sound faults: wind, rumble, hum, hiss, from the analysis bands.
            let mut audio_findings: Vec<(f64, String, String, String)> = Vec::new();
            for (path, name, at, fixes) in &audio_media {
                // A library track or effect is what it is: its low end is
                // not a hum to clean.
                if klipp_analysis::is_licensed_audio(path) {
                    continue;
                }
                if let Some(au) = klipp_analysis::report::MediaAnalysis::load(path).audio {
                    // The analysis reads the file; a fault the clips already
                    // carry the fix for is not asked for again (#505).
                    for (what, fix) in au.faults().into_iter().filter(|(what, _)| !fixes.covers(what)) {
                        audio_findings.push((*at, name.clone(), what, fix));
                    }
                }
            }

            // The headline counts everything the check found, colour
            // and sound included. It used to count only the craft
            // findings, so a timeline with ten colour errors under it
            // was announced as "0 errors, 0 warnings" and `next` said
            // "Clean. Export." — which is what an agent reads.
            let colour_errors = color_findings.iter().filter(|f| f.0 == "error").count();
            let errors = report.errors + colour_errors;
            let warnings = report.warnings + (color_findings.len() - colour_errors) + audio_findings.len();
            let next = if errors > 0 {
                "Fix the errors before you deliver — each finding says what and where. Then check again."
            } else if warnings > 0 {
                "No errors. Decide each warning: fix it, or keep it on purpose and be able to say why. Then export."
            } else {
                "Clean. Export at the brief's loudness, poll export_status, then save_project."
            };
            let mut text = report.text();
            if errors != report.errors || warnings != report.warnings {
                let notes = report.findings.len() - report.errors - report.warnings;
                let head = format!(
                    "Checked {:.2} s: {errors} error{}, {warnings} warning{}, {notes} note{}.",
                    report.duration_secs,
                    if errors == 1 { "" } else { "s" },
                    if warnings == 1 { "" } else { "s" },
                    if notes == 1 { "" } else { "s" },
                );
                match text.find('\n') {
                    Some(i) => text.replace_range(..i, &head),
                    None => text = head,
                }
            }
            if color {
                let read = color_times.len() - unread.len();
                text.push_str(&format!("\n\nColour (measured in Lab on {read} shot{}, {shots_on_target} on target):", if read == 1 { "" } else { "s" }));
                if color_findings.is_empty() {
                    text.push_str(" nothing amiss.");
                } else {
                    for f in &color_findings {
                        text.push_str(&format!("\n[{}] {:.2} s  {}. {}", f.0, f.1, f.2, f.3));
                    }
                }
                // A group that leaves the look on purpose is said once, not
                // as a taste finding on every shot of it (#793).
                for name in &departing {
                    text.push_str(&format!("\n[note] grade group \"{name}\" leaves the look on purpose: its shots are judged for faults only (clipping, crushed blacks, faces, noise), not for its colour or contrast"));
                }
                // A shot that could not be read is not one that passed.
                if !unread.is_empty() {
                    text.push_str(&format!("\n[warning] {} shot{} could not be read, so are not judged: {}", unread.len(), if unread.len() == 1 { "" } else { "s" }, unread.join("; ")));
                }
            }
            if !audio_findings.is_empty() {
                text.push_str("\n\nSound:");
                for f in &audio_findings {
                    text.push_str(&format!("\n[warning] {}: {}. {}", f.1, f.2, f.3));
                }
            }
            let findings_json: Vec<serde_json::Value> = color_findings.iter().map(|f| json!({ "severity": f.0, "rule": "color", "at_secs": f.1, "what": f.2, "hint": f.3 })).collect();
            let audio_json: Vec<serde_json::Value> = audio_findings.iter().map(|f| json!({ "severity": "warning", "rule": "sound", "at_secs": f.0, "media": f.1, "what": f.2, "hint": f.3 })).collect();
            Ok(json!({ "text": text, "errors": errors, "warnings": warnings, "findings": report.findings, "color_findings": findings_json, "departing_groups": departing, "shots_on_target": shots_on_target, "sound_findings": audio_json, "duration_secs": report.duration_secs, "revision": ctx.shared.revision(), "next": next }))
        }

        // Said what they did (#741): the step taken back or put back, as
        // the history names it, and what the next one would be.
        which @ (ControlCommand::Undo | ControlCommand::Redo) => {
            let undo = matches!(which, ControlCommand::Undo);
            let labels = || ctx.shared.project.lock().map(|p| (p.undo.clone(), p.redo.clone())).unwrap_or_default();
            let (was_undo, was_redo) = labels();
            let step = if undo { was_undo } else { was_redo };
            let mut v = send(if undo { AppCommand::Undo } else { AppCommand::Redo })?;
            let Some(step) = step else {
                v["text"] = json!(if undo { "Nothing to undo." } else { "Nothing to redo." });
                return Ok(v);
            };
            let (next_undo, next_redo) = labels();
            let mut text = format!("{} \u{201c}{step}\u{201d}.", if undo { "Undid" } else { "Redid" });
            match (&next_undo, &next_redo) {
                (Some(u), _) if undo => text.push_str(&format!(" Next undo would take back \u{201c}{u}\u{201d}.")),
                (_, Some(r)) if !undo => text.push_str(&format!(" Next redo would put back \u{201c}{r}\u{201d}.")),
                _ if undo => text.push_str(" Nothing more to undo."),
                _ => text.push_str(" Nothing more to redo."),
            }
            v[if undo { "undid" } else { "redid" }] = json!(step);
            v["next_undo"] = json!(next_undo);
            v["next_redo"] = json!(next_redo);
            v["text"] = json!(text);
            Ok(v)
        }

        ControlCommand::Batch { commands } => {
            if in_batch {
                return Err("a batch cannot contain a batch".into());
            }
            if commands.is_empty() {
                return Err("an empty batch".into());
            }
            let mut buf = Some(Collected::default());
            for (i, c) in commands.into_iter().enumerate() {
                // Every command checked and translated before any is sent:
                // a bad one refuses the batch whole.
                handle(ctx, c, &mut buf).map_err(|e| format!("batch command {i}: {e}"))?;
            }
            let Collected { commands: cmds, ops } = buf.take().unwrap_or_default();
            if ops.is_empty() {
                return send_envelope(ctx, cmds, true);
            }
            // Looks are resolved edits and apply as one history entry of
            // their own; with other writes beside them the batch would be
            // two undo steps, not the one it promises.
            if !cmds.is_empty() {
                return Err("a batch with looks holds only looks; send the other writes in a batch of their own".into());
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let label = if ops.len() == 1 { "look".to_string() } else { format!("look on {} clips", ops.len()) };
            let mut v = send_full_envelope(ctx, cmds, edits, true, Some(revision), Some(label))?;
            v["text"] = json!(plan.text_folded());
            Ok(v)
        }

        ControlCommand::Export { path, width, height, fps, crf, bit_depth, target_lufs, true_peak_dbtp, burn_captions } => {
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
            let mut settings = klipp_core::ExportSettings { output_path: out.clone(), format, ..Default::default() };
            // The default H.264 and AAC do not go in WebM.
            settings.fit_codecs_to_format();
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
            if target_lufs.is_some() {
                settings.target_lufs = target_lufs;
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
            });
            let burnt = said["captions_and_titles_burnt_in"].as_bool().unwrap_or(true);
            let known_jobs: std::collections::HashSet<String> = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner())
                .iter().map(|job| job.job_id.clone()).collect();
            let mut v = send(AppCommand::ExportSequence { settings })?;
            v["export"] = said;
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

    /// A video track above the footage holding no picture (a title, a
    /// gap) or hidden is passed over: the colour tools grade the track
    /// the footage is on (#513).
    #[test]
    fn the_picture_track_is_the_top_one_with_footage() {
        use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
        let tc = |s: i64| klipp_core::Timecode::from_frames(s * 25, 25);
        let media = || Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0), tc(2)) };
        let mut empty = Track::new("Credits", TrackKind::Video);
        empty.clips = vec![Clip::gap(tc(0), tc(2))];
        let mut hidden = Track::new("Hidden", TrackKind::Video);
        hidden.clips = vec![media()];
        hidden.visible = false;
        let mut footage = Track::new("Video 1", TrackKind::Video);
        footage.clips = vec![media()];
        let mut seq = Sequence::new_default();
        seq.tracks = vec![hidden, empty, footage, Track::new("Audio 1", TrackKind::Audio)];
        assert_eq!(picture_track(&seq).map(|t| t.name.as_str()), Some("Video 1"));
        seq.tracks.retain(|t| t.name != "Video 1");
        assert!(picture_track(&seq).is_none(), "a hidden track and a track of gaps hold nothing to grade");
    }

    /// An update changes what it names and nothing else, clears what it
    /// gives as null, and refuses a name the brief does not have (#498).
    #[test]
    fn an_update_changes_only_the_fields_it_names() {
        let current: klipp_timeline::Brief = serde_json::from_value(serde_json::json!({
            "title": "Wherever We Walk", "tone": "calm", "target_secs": 150.0,
            "cast": [{"name": "Tove", "role": "walking in Bangkok"}], "notes": "sparse voiceover",
        })).unwrap();
        let updated = merge_brief(&current, &serde_json::json!({ "notes": "stylised transitions at the chapter changes", "tone": null })).unwrap();
        assert_eq!(updated.notes, "stylised transitions at the chapter changes");
        assert_eq!(updated.tone, "", "null clears");
        assert_eq!(updated.title, "Wherever We Walk");
        assert_eq!(updated.target_secs, Some(150.0));
        assert_eq!(updated.cast.len(), 1);

        let err = merge_brief(&current, &serde_json::json!({ "notse": "x" })).unwrap_err();
        assert!(err.contains("\"notse\" is not a field of the brief"), "{err}");
        assert!(merge_brief(&current, &serde_json::json!(["notes"])).is_err());
        // The type styles' motion is held to its fields as in set_brief.
        assert!(merge_brief(&current, &serde_json::json!({
            "type_styles": [{"name": "place", "enter": {"kind": "rise", "delai_secs": 1.0}}]
        })).is_err());
    }

    #[test]
    fn an_apply_without_a_label_is_named_after_its_ops() {
        use klipp_document::ops::{Anchor, ClipRef, Op};
        let cut = Op::Cut { from: Anchor::Secs(1.0), to: Anchor::Secs(2.0) };
        let split = Op::Split { clip_id: ClipRef::At { track: 0, secs: 0.5 }, at: Anchor::Secs(1.0) };
        assert_eq!(apply_label(std::slice::from_ref(&cut)), "Cut");
        assert_eq!(apply_label(&[cut, split.clone(), split]), "Cut and 2 more");
        assert_eq!(apply_label(&[]), "Apply");
    }
}

#[cfg(test)]
mod face_region_tests {
    use super::face_or_head_regions;
    use klipp_analysis::framing::{FaceBox, FrameSample, Framing};

    fn walking_away() -> FrameSample {
        FrameSample { secs: 1.0, people: vec![(0.4, 0.2, 0.2, 0.6)], faces: Vec::new() }
    }

    /// #326: the face detector looked, saw a person and no face — their
    /// back is to the camera. No head stands in for the face.
    #[test]
    fn a_person_with_no_face_in_view_gives_no_face() {
        let framing = Framing { face_model: Some("yunet_face_2023mar".into()), ..Default::default() };
        assert!(face_or_head_regions(&framing, &walking_away()).is_empty());
    }

    #[test]
    fn a_detected_face_is_the_face() {
        let framing = Framing { face_model: Some("yunet_face_2023mar".into()), ..Default::default() };
        let mut s = walking_away();
        s.faces.push(FaceBox { x: 0.45, y: 0.22, w: 0.1, h: 0.12, score: 0.9, landmarks: Vec::new() });
        assert_eq!(face_or_head_regions(&framing, &s), vec![[0.45, 0.22, 0.1, 0.12]]);
    }

    /// An analysis made without the face detector still reads the head,
    /// as before: it cannot tell a face from a back.
    #[test]
    fn without_a_face_detector_the_head_stands_in() {
        let framing = Framing::default();
        assert_eq!(face_or_head_regions(&framing, &walking_away()).len(), 1);
    }
}

#[cfg(test)]
mod fps_fraction_tests {
    #[test]
    fn rates_become_fractions() {
        assert_eq!(super::fps_fraction(25.0), Ok((25, 1)));
        assert_eq!(super::fps_fraction(29.97), Ok((30000, 1001)));
        assert_eq!(super::fps_fraction(23.976), Ok((24000, 1001)));
        assert_eq!(super::fps_fraction(59.94), Ok((60000, 1001)));
        assert!(super::fps_fraction(0.0).is_err());
        assert!(super::fps_fraction(25.5).is_err());
    }
}

#[cfg(test)]
mod png_size_tests {
    /// A whole PNG gives its size; a cut-off one, or not a PNG, nothing.
    #[test]
    fn a_whole_png_gives_its_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&360u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        let whole = [png.clone(), vec![0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]].concat();
        assert_eq!(super::png_size_if_whole(&whole), Some((640, 360)));
        assert_eq!(super::png_size_if_whole(&png), None, "no IEND yet");
        assert_eq!(super::png_size_if_whole(b"not a png at all, no, not one bit of it"), None);
    }
}

#[cfg(test)]
mod audio_fixes_tests {
    use super::AudioFixes;

    fn fx(t: &str, params: &[(&str, f32)]) -> klipp_core::AudioEffectInstance {
        klipp_core::AudioEffectInstance { effect_type: t.into(), enabled: true, wet_dry: 1.0, params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
    }

    /// #505: after clean_audio wind the rumble is not asked for again,
    /// a hum still is; a sound also heard where nothing cleaned it still
    /// asks for the fix.
    #[test]
    fn a_fix_on_every_clip_that_plays_the_sound_answers_its_fault() {
        let wind = AudioFixes::on(&[fx("denoiser", &[("strength", 0.6), ("wind", 0.8)])]);
        assert!(wind.covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        assert!(!wind.covers("a mains hum near 50/60 Hz runs under the sound"));
        assert!(AudioFixes::on(&[fx("hum_removal", &[])]).covers("a mains hum near 50/60 Hz runs under the sound"));
        let bare = AudioFixes::on(&[]);
        assert!(!wind.and(bare).covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        assert!(wind.and(AudioFixes::ALL).covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        let off = klipp_core::AudioEffectInstance { enabled: false, ..fx("voice_isolation", &[]) };
        assert!(!AudioFixes::on(&[off]).covers("high-frequency hiss or air is loud against the sound"));
    }
}

#[cfg(test)]
mod group_look_tests {
    use klipp_color::measure::LookTarget;
    use klipp_timeline::{Clip, GradeGroup, Sequence, Track, TrackKind};

    /// #793: a shot in a group with its own look is held to it, a shot in
    /// a group that leaves the look to faults only, any other to the brief.
    #[test]
    fn a_group_holds_its_shots_to_its_own_look() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
        let id = klipp_core::MediaId::new();
        let mut seq = Sequence::new_default();
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = (0..3).map(|k| Clip::media(id, tc(k as f64 * 2.0), tc(k as f64 * 2.0 + 2.0), tc(0.0), tc(2.0))).collect();
        seq.tracks = vec![v];
        let mut south = GradeGroup::new("South");
        south.look = Some(LookTarget { name: "warm_interview".into(), ..Default::default() });
        let mut north = GradeGroup::new("North");
        north.look = Some(LookTarget::preset("cool_tech").unwrap());
        north.departs = true;
        seq.tracks[0].clips[0].grade_group = Some(south.id);
        seq.tracks[0].clips[1].grade_group = Some(north.id);
        seq.grade_groups = vec![south, north];

        let south_look = super::group_look(&seq, tc(1.0)).expect("the south group's look");
        assert_eq!(south_look, LookTarget::preset("warm_interview").unwrap(), "a look by name takes the preset's numbers");
        assert_eq!(super::group_look(&seq, tc(3.0)), Some(super::departing_look()), "leaving the look on purpose outranks its own look");
        assert_eq!(super::group_look(&seq, tc(5.0)), None, "a shot in no group goes by the brief");
    }

    /// #811 review: a frame warm on purpose in a group that leaves the look
    /// gets no cast finding, though the unbriefed target gives it one; a
    /// clipped frame there is still a fault.
    #[test]
    fn a_group_off_the_look_is_judged_for_faults_only() {
        use klipp_color::measure::{judge, FrameMeasure, Verdict};
        let warm = FrameMeasure { l_p1: 6.0, l_p50: 55.0, l_p99: 90.0, cast_a: 4.0, cast_b: 14.0, neutral_pct: 20.0, neutral_solid: true, ..FrameMeasure::default() };
        let rules = |m: &FrameMeasure, t: &LookTarget| judge(m, t).into_iter().filter(|f| f.verdict != Verdict::Pass).map(|f| f.rule).collect::<Vec<_>>();
        assert!(rules(&warm, &LookTarget::unbriefed()).contains(&"cast"), "the unbriefed target calls the warmth a cast");
        assert!(!rules(&warm, &super::departing_look()).contains(&"cast"), "{:?}", rules(&warm, &super::departing_look()));
        let clipped = FrameMeasure { clip_hi_pct: 6.0, ..warm.clone() };
        assert!(rules(&clipped, &super::departing_look()).contains(&"clipped_white"));
    }
}

#[cfg(test)]
mod scene_group_tests {
    /// #772 review: shots of two files that share a name are two scenes,
    /// and a file with one shot is no scene to group.
    #[test]
    fn same_name_different_files_are_two_scenes() {
        let (a, b) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
        let shot = |id: &str| (id.to_string(), 0.0, "IMG_0001.mov".to_string());
        let shots = vec![shot("1"), shot("2"), shot("3"), shot("4"), shot("5")];
        let media_of = [("1", a), ("2", a), ("3", b), ("4", b), ("5", klipp_core::MediaId::new())].into_iter().map(|(k, m)| (k.to_string(), m)).collect();
        let groups = super::scene_groups(&shots, &media_of);
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0]["clips"], serde_json::json!(["1", "2"]));
        assert_eq!(groups[1]["clips"], serde_json::json!(["3", "4"]));
        assert_ne!(groups[0]["media_id"], groups[1]["media_id"]);
    }
}

#[cfg(test)]
mod grade_limit_tests {
    use super::{describe_step, GradeLimits};
    use klipp_color::measure::PrimaryStep;

    /// #742: a step is held within the strength's reach of where the shot
    /// started, however many rounds ask for more, and says which knobs
    /// were held.
    #[test]
    fn knobs_stay_within_reach_of_where_the_shot_started() {
        let start = PrimaryStep { saturation: 1.0, ..Default::default() };
        let greedy = PrimaryStep { saturation: 0.3, temperature: 5.0, exposure: 0.1, ..Default::default() };
        let gentle = GradeLimits::named("gentle").unwrap();
        let (one, held) = gentle.clamp(&start, &start, &greedy);
        assert!((one.saturation - 1.12).abs() < 1e-4 && held == vec!["saturation"], "{one:?} {held:?}");
        // A second round asking for the same is held where the first was.
        let (two, held) = gentle.clamp(&start, &one, &greedy);
        assert!((two.saturation - 1.12).abs() < 1e-4 && (two.temperature - 8.0).abs() < 1e-4, "{two:?}");
        assert_eq!(held, vec!["temperature", "saturation"]);
        let bold = GradeLimits::named("bold").unwrap();
        assert!((bold.clamp(&start, &start, &greedy).0.saturation - 1.3).abs() < 1e-4, "bold reaches the 1.3x gentle refused");
        assert!(GradeLimits::named("wild").is_err());
        assert_eq!(describe_step(&start, &one), "exposure +0.10, temperature +5.00, saturation x1.12");
    }
}
