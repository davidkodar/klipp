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
                    Ok(v) => Response::ok(id, with_project_generation(&ctx, v)),
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
    let presented = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_presented();
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
    fn a_read_during_playback_seeks_back_and_plays_on() {
        let t = Timecode::from_frames(90, 30);
        let cmds = restore_commands(HeldTransport { at: Some(t), playing: true });
        assert!(matches!(cmds.as_slice(), [AppCommand::SeekTo(s), AppCommand::Play] if *s == t), "{cmds:?}");
    }
}

/// Seek to `secs`, wait for the frame there, read its RGBA bytes, and
/// put the user's playhead back — the picture as the viewer shows it,
/// for measuring. Shares the one-grab lock and the wait with frame_at.
fn grab_frame_rgba(ctx: &ControlContext, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
    let held = hold_transport(ctx)?;
    let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
    let target = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
    let before = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_generation();
    send_envelope(ctx, vec![AppCommand::Pause, AppCommand::SeekTo(target)], false)?;
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
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
            restore_transport(ctx, held);
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
        if width > 0 && height > 0 && data.len() >= expected {
            break (data, width, height);
        }
        if std::time::Instant::now() >= deadline {
            restore_transport(ctx, held);
            return Err("the viewer has no frame to measure yet".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    restore_transport(ctx, held);
    let expected = (width as usize) * (height as usize) * 4;
    Ok((data[..expected].to_vec(), width, height))
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
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
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
/// source tree at build time. Mirrors the app's own lookup.
fn bundled_luts_dir() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("assets").join("luts");
            if c.exists() {
                return Some(c);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(|p| p.parent()).map(|p| p.join("assets").join("luts"));
    src.filter(|p| p.exists())
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
    let ql = q.to_lowercase();
    let tokens: Vec<&str> = ql.split_whitespace().collect();
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
            // An exact stem match settles it; otherwise ask to be specific.
            if let Some(exact) = matches.iter().find(|(_, s, _)| s.to_lowercase() == ql) {
                return Ok((exact.2.clone(), exact.1.clone()));
            }
            let names: Vec<String> = matches.iter().take(10).map(|(c, s, _)| format!("{c}/{s}")).collect();
            Err(format!("\"{q}\" matches {} looks; be more specific: {}", matches.len(), names.join(", ")))
        }
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
        if mx >= 254.0 {
            clip_hi += 1;
        }
        if mn <= 1.0 {
            clip_lo += 1;
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
    let grey = (rb + gb + bb) / 3.0;
    // The cast, if any: which channel leads and by how much, in percent
    // of the grey level — what "warm" or "cool" means as a number.
    let cast = |name: &'static str, v: f64| (name, ((v - grey) / grey.max(1e-6) * 100.0) as f32);
    let mut casts = [cast("red", rb), cast("green", gb), cast("blue", bb)];
    casts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let balance_note = if casts[0].1 - casts[2].1 < 4.0 {
        "neutral".to_string()
    } else {
        format!("{} cast, {:.0}% over {}", casts[0].0, casts[0].1 - casts[2].1, casts[2].0)
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
         RGB balance: {balance_note}\n\
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
    let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
    let plan = klipp_document::ops::resolve_with(&seq, &pool, ops, &AnalysedTranscripts).map_err(|r| format!("{r} (step {} refused; nothing applied)", r.step + 1))?;
    Ok((plan, revision))
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

    fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).audio.map(|a| a.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default()
    }

    fn shot_changes(&self, item: &klipp_media::MediaItem) -> Vec<f64> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).shots.map(|s| s.boundaries).unwrap_or_default()
    }

    fn screen_sensitive(&self, item: &klipp_media::MediaItem) -> Vec<klipp_document::ops::SensitiveText> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).screentext.map(|st| st.sensitive.into_iter().map(|s| klipp_document::ops::SensitiveText { kind: s.kind, masked: s.masked, start_secs: s.start_secs, end_secs: s.end_secs, x: s.x, y: s.y, w: s.w, h: s.h }).collect()).unwrap_or_default()
    }

    fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        klipp_analysis::report::MediaAnalysis::load(&item.path).framing.map(|f| f.samples.iter().flat_map(|s| face_or_head_regions(s).into_iter().map(move |b| (s.secs, b))).collect()).unwrap_or_default()
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

/// What to do after a batch of ops was applied, from the ops in it: the
/// reply points at the next step so a session follows a trail rather
/// than recalling a recipe.
fn next_after_ops(ops: &[klipp_document::ops::Op]) -> Option<&'static str> {
    use klipp_document::ops::Op;
    let has = |f: &dyn Fn(&Op) -> bool| ops.iter().any(f);
    if has(&|o| matches!(o, Op::Redact { .. })) {
        Some("Redacted. Look at a frame inside the stretch (frame_at) to see the box covers what it should — the copy on the track above hides it — then check again.")
    } else if has(&|o| matches!(o, Op::Place { with_audio: true, .. })) && ops.len() >= 2 {
        Some("The selects are down. Where two from the same set-up meet is a jump cut: cover the joins with B-roll on a track above (place with_audio false), or j_cut the sound. Then captions, then check.")
    } else if has(&|o| matches!(o, Op::Captions { .. })) {
        Some("Captions are down. Read the cues in the reply and correct any the transcript misheard with set_text. Then check, and export with the brief's loudness.")
    } else if has(&|o| matches!(o, Op::LowerThird { .. })) {
        Some("The lower third is placed. Look at a frame with frame_at to see it sits clear of anything burnt into the picture, then move it with style if not.")
    } else if has(&|o| matches!(o, Op::Duck { .. } | Op::Audio { .. })) {
        Some("The sound is set. Check holds the mix to the craft; then export at the brief's loudness.")
    } else if has(&|o| matches!(o, Op::Place { .. })) {
        Some("Placed. Look with frame_at or contact_sheet, and run check when the cut is together.")
    } else {
        Some("Run check to hold the cut to the brief and the craft before you export.")
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
/// A sample's faces as boxes, else its people's heads.
fn face_or_head_regions(s: &klipp_analysis::framing::FrameSample) -> Vec<[f32; 4]> {
    if !s.faces.is_empty() {
        s.faces.iter().map(|f| [f.x, f.y, f.w, f.h]).collect()
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
    let source = clip.source_in.to_secs_f64() + (secs - clip.timeline_in.to_secs_f64()).max(0.0) * clip.speed.max(0.001);
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
    let speed = clip.speed.max(0.001);
    let mut keys: Vec<klipp_document::ops::WindowKey> = framing
        .samples
        .iter()
        .filter(|s| s.secs >= s_in && s.secs < s_out)
        .filter_map(|s| {
            // The biggest face in the sample: the subject. A window on
            // a face in the background would leave the subject's own
            // face outside the correction.
            let b = face_or_head_regions(s).into_iter().max_by(|a, b| (a[2] * a[3]).total_cmp(&(b[2] * b[3])))?;
            Some(klipp_document::ops::WindowKey { at: (s.secs - s_in) / speed, x: b[0], y: b[1], w: b[2], h: b[3] })
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
fn measure_clip(ctx: &ControlContext, from: f64, to: f64, timeout_ms: Option<u64>) -> Result<(Vec<(f64, klipp_color::measure::FrameMeasure)>, Option<String>), String> {
    let span = (to - from).max(0.05);
    let times: Vec<f64> = if span < 0.6 {
        vec![from + span / 2.0]
    } else {
        [0.2, 0.5, 0.8].iter().map(|f| from + span * f).collect()
    };
    let mut out = Vec::new();
    for t in times {
        match measure_at(ctx, t, timeout_ms) {
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
fn measure_at(ctx: &ControlContext, secs: f64, timeout_ms: Option<u64>) -> Result<klipp_color::measure::FrameMeasure, String> {
    let (data, w, h) = grab_frame_rgba(ctx, secs, timeout_ms)?;
    let faces = skin_regions(ctx, secs, &data, w, h);
    Ok(klipp_color::measure::measure_frame(&data, w, h, &faces))
}

/// The look a frame is judged against: the brief's, else neutral; a
/// screen recording (the beat sheet found holds) is judged as one when
/// the brief names no look.
fn look_for(ctx: &ControlContext, secs: f64) -> klipp_color::measure::LookTarget {
    use klipp_color::measure::LookTarget;
    if let Ok(seq) = ctx.sequence.read() {
        if let Some(look) = seq.brief.as_ref().and_then(|b| b.look.clone()) {
            return resolve_look_target(look);
        }
        let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
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
    LookTarget::preset("broadcast_neutral").unwrap()
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

/// The clip's Corrector knobs as they are: what a grade step adds to.
fn corrector_now(clip: &klipp_timeline::Clip) -> klipp_color::measure::PrimaryStep {
    let mut s = klipp_color::measure::PrimaryStep { saturation: 1.0, ..Default::default() };
    if let Some(n) = clip.node_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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

/// A short hash of a text, for a file name that follows its words.
fn seahash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Seconds since the epoch, for a file name.
fn chrono_stamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The voice's model and settings files from the model cache, downloaded
/// when asked and not there: the voice `id` in the manifest and its
/// `<id>_config` companion.
fn voice_files(voice: &str, download: bool) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let ids = [voice.to_string(), format!("{voice}_config")];
    let mut paths = Vec::new();
    for id in &ids {
        let entry = manifest.get(id).ok_or_else(|| {
            let voices: Vec<&str> = manifest.models.iter().filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config")).map(|e| e.id.as_str()).collect();
            format!("no voice {id} in the manifest; the voices are {}", voices.join(", "))
        })?;
        let p = cache.path_for(entry);
        if !p.is_file() {
            if !download {
                return Err(format!("the voice {voice} is not downloaded ({} MB); ask with download_models true, or download it in Preferences", entry.size_bytes / 1_000_000));
            }
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut progress = klipp_ml::download::NoOpProgress;
            klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
        }
        paths.push(cache.path_for(entry));
    }
    Ok((paths.remove(0), paths.remove(0)))
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
                    "unsaved_changes_from": if project.dirty { project.changed_by.clone().or(Some("edits".into())) } else { None },
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

        ControlCommand::AutoSfx { whoosh, click, riser, apply } => {
            if in_batch {
                return Err("auto_sfx is its own batch; send it on its own".into());
            }
            let synthesized = whoosh.is_none() && click.is_none() && riser.is_none();
            if synthesized && !apply {
                let seq = ctx.sequence.read().unwrap_or_else(|e| e.into_inner());
                let placements = klipp_document::sound_design::preview_whoosh(&seq, f64::from(klipp_audio::WhooshVariant::Short.duration_secs()));
                let summary: Vec<_> = placements.iter().map(|p| format!("whoosh at {:.2} s", p.at_secs)).collect();
                return Ok(json!({ "text": format!("Would synthesize a short whoosh and place {} sounds: {}. Run auto_sfx with apply true to create the audio and place it.", summary.len(), summary.join("; ")), "placements": summary, "applied": false, "synthesized_whoosh": false }));
            }
            let whoosh = if synthesized {
                // Materialize and import the reusable asset only when applying.
                // Serializing this small path
                // avoids two socket clients writing/importing the same WAV.
                static SYNTH_IMPORT: Mutex<()> = Mutex::new(());
                let _guard = SYNTH_IMPORT.lock().unwrap_or_else(|e| e.into_inner());
                let dir = klipp_media::disk_cache::cache_base_dir().join("auto-sfx");
                std::fs::create_dir_all(&dir).map_err(|e| format!("create SFX cache: {e}"))?;
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
                let kit = klipp_document::sound_design::SfxKit { whoosh, click, riser };
                let placements = klipp_document::sound_design::placements(&seq, &pool, kit, &reveals, &loudest);
                let ops = klipp_document::sound_design::ops(&seq, &placements);
                (placements, ops)
            };
            if ops.is_empty() {
                return Ok(json!({ "text": "Nothing to place: no cut on the top picture track without an effect there already, and no reveal inside its clips (analyze gives the beat sheet).", "steps_resolved": 0, "applied": false }));
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
            let found = klipp_analysis::spectrogram::findings(&spec);
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
                    "The sound of {filename}{}, {:.1} s read in {:.2} s columns:\n{}\n\nEvery threshold here is a level *relative to the voice*, never an absolute one: a quiet recording of a clean room and a loud recording of a noisy one have the same absolute noise floor and different problems. Under about 30 dB below the voice a steady noise stops being audible under speech; at 20 dB down it is plainly there.",
                    if from > 0.0 || to.is_some() { format!(" from {:.1} s", from) } else { String::new() },
                    spec.duration_secs,
                    spec.hop_secs,
                    if lines.is_empty() { "Nothing to report: no band comes close enough to the voice to be heard under it.".to_string() } else { lines.join("\n") },
                ),
                "media_id": media_id,
                "filename": filename,
                "duration_secs": spec.duration_secs,
                "media_duration_secs": dur,
                "hop_secs": spec.hop_secs,
                "rate": spec.rate,
                "findings": found.iter().map(|(sev, what, fix)| json!({ "severity": sev, "what": what, "fix": fix })).collect::<Vec<_>>(),
                "bands": bands,
                "next": "clean_audio on the clip whose sound plays takes wind, hiss, hum and deess; run audio_spectrum again to see whether it worked.",
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
                    let (path, filename, dur) = {
                        let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                        let item = pool.get(&mid).ok_or_else(|| format!("no media item {}", mid.0))?;
                        (item.path.clone(), item.filename.clone(), item.duration.to_secs_f64())
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
                                .and_then(|f| f.people_at(t, 1.5))
                                .map(face_or_head_regions)
                                .unwrap_or_default();
                            measures.push(klipp_color::measure::measure_frame(&data, w, h, &faces));
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
                        seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).take(1)
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }))
                            .map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into())))
                            .take(24)
                            .collect()
                    };
                    if shots.is_empty() {
                        return Err("no picture clip on the top video track to take a look from".into());
                    }
                    // The shot already closest to a defensible picture:
                    // the fewest findings, and a face in it if any shot
                    // has one, since a face is what the rest is judged
                    // against.
                    let mut best: Option<(f32, klipp_color::measure::FrameMeasure, String, f64)> = None;
                    for (from, to, name) in &shots {
                        let (frames, _) = measure_clip(ctx, *from, *to, Some(6000))?;
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
                seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).take(1)
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }))
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
                return Err("matching needs at least two shots on the top video track".into());
            }
            // Every shot read the way a colourist checks one: three
            // frames, the middle reading grading it.
            let mut read: Vec<(usize, f64, klipp_color::measure::FrameMeasure)> = Vec::new();
            for (i, (id, from, to, _)) in shots.iter().enumerate() {
                let _ = id;
                let (frames, _drift) = measure_clip(ctx, *from, *to, Some(6000))?;
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
                            send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Match Scene".into()))?;
                            done += 1;
                            read[i].2 = measure_at(ctx, read[i].1, Some(6000))?;
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
                    if apply { "" } else { " Run it with apply true to grade each shot onto its scene's hero, one undo step a round." },
                ),
                "scenes": out_scenes,
                "joins": joins,
                "applied": apply,
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
                let chain = c.node_graph.chain();
                let faults = c.node_graph.chain_out_of_order();
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
            let (mid, name) = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let id = klipp_core::ClipId(clip_id.parse().map_err(|e| format!("clip_id is not a uuid: {e}"))?);
                let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).ok_or_else(|| format!("no clip {clip_id}"))?;
                let item = clip.media_id().and_then(|m| pool.get(&m));
                let name = item.map(|it| it.filename.clone()).unwrap_or_else(|| "the clip".into());
                let (t_in, t_out) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
                let speed = clip.speed.max(0.001);
                let biggest = item
                    .and_then(|it| klipp_analysis::report::MediaAnalysis::load(&it.path).framing)
                    .and_then(|f| {
                        f.samples
                            .iter()
                            .filter(|s| s.secs >= s_in && s.secs < s_out)
                            .filter_map(|s| {
                                let area = face_or_head_regions(s).into_iter().map(|b| b[2] * b[3]).fold(0.0f32, f32::max);
                                (area > 0.0).then_some((s.secs, area))
                            })
                            .max_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(secs, _)| t_in + (secs - s_in) / speed)
                    })
                    .filter(|t| *t >= t_in && *t < t_out);
                (biggest.unwrap_or((t_in + t_out) / 2.0), name)
            };
            let look = look_for(ctx, mid);
            let target = look.skin_hue.unwrap_or(SKIN_HUE_CENTRE);
            let mut rounds = Vec::new();
            let mut shift = 0.0f32;
            let mut m = measure_at(ctx, mid, Some(6000))?;
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
                send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Skin Balance".into()))?;
                m = measure_at(ctx, mid, Some(6000))?;
                let after = m.faces.iter().find(|f| f.patches >= 4).map(|f| json!({ "hue": f.hue, "chroma": f.chroma }));
                rounds.push(json!({ "round": round + 1, "hue_shift": shift, "after": after }));
            }
            let face_now = m.faces.iter().find(|f| f.readable());
            let text = match face_now {
                Some(f) => format!("{name} at {mid:.2} s: skin hue {:.0}° → {:.0}° (band {SKIN_HUE_LOW:.0}–{SKIN_HUE_HIGH:.0}°, aim {target:.0}°), chroma {:.0}, L* {:.0}, {} of {}×{} patches{}{}", face0.hue, f.hue, f.chroma, f.median.l, f.patches, f.grid, f.grid, if apply { format!("; {} round(s), skin secondary hue shift {shift:+.0}°{}", rounds.len(), match windowed { 0 => ", keyed on hue alone — no face track in this clip's analysis, so anything else in the skin band turns with the face".to_string(), 1 => ", held inside a window on the face".to_string(), n => format!(", held inside a window tracked on the face over {n} samples") }) } else { String::new() }, before.get("advice").and_then(|a| a.as_str()).map(|a| format!(". {a}")).unwrap_or_default()),
                None => format!("{name}: the face could not be read after the change"),
            };
            Ok(json!({ "text": text, "before": before, "rounds": rounds, "hue_shift": shift, "applied": apply && !rounds.is_empty(), "revision": ctx.shared.revision(), "next": if apply { "frame_at to see it; check with color reads every shot." } else { "Run it with apply true to turn the skin onto the band as one undo step per round." } }))
        }

        ControlCommand::GradeToTarget { clip_id, apply, rounds, force } => {
            if in_batch {
                return Err("grade_to_target measures and applies; send it on its own".into());
            }
            let shots: Vec<(String, f64, String)> = {
                let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
                let want = clip_id.as_deref().map(|s| s.parse::<uuid::Uuid>().map_err(|e| format!("clip_id is not a uuid: {e}"))).transpose()?;
                seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).take(1)
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }) && want.is_none_or(|w| c.id.0 == w))
                    .map(|c| (c.id.0.to_string(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0, c.media_id().and_then(|m| pool.get(&m)).map(|it| it.filename.clone()).unwrap_or_else(|| "clip".into())))
                    .take(48)
                    .collect()
            };
            if shots.is_empty() {
                return Err("no picture clip to grade on the top video track".into());
            }
            let n_rounds = if apply { rounds.unwrap_or(3).clamp(1, 5) } else { 0 };
            let mut per_shot = Vec::new();
            let mut lines = Vec::new();
            for (id, mid, name) in &shots {
                let look = look_for(ctx, *mid);
                // Three frames across the shot, not one: the middle
                // reading grades it and the drift between them is said.
                let span = shot_span(ctx, id).unwrap_or((*mid - 0.5, *mid + 0.5));
                let (reads, drift) = measure_clip(ctx, span.0, span.1, Some(6000))?;
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
                for _ in 0..n_rounds {
                    let fails = colour_report(&m, &look, *mid);
                    let moving = step.exposure.abs() > 0.03 || step.lift.abs() > 0.01 || step.gain.abs() > 0.01 || step.temperature.abs() > 0.5 || step.tint.abs() > 0.5 || step.saturation.abs() > 0.03;
                    if fails.is_empty() || !moving {
                        break;
                    }
                    let now = {
                        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                        let cid = klipp_core::ClipId(id.parse().map_err(|e| format!("{e}"))?);
                        seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).map(corrector_now).unwrap_or_default()
                    };
                    let op: klipp_document::ops::Op = serde_json::from_value(json!({
                        "op": "grade", "clip_id": id,
                        "exposure": (now.exposure + step.exposure).clamp(-6.0, 6.0),
                        "lift": (now.lift + step.lift).clamp(-1.0, 1.0),
                        "gain": (now.gain + step.gain).clamp(-1.0, 1.0),
                        "temperature": (now.temperature + step.temperature).clamp(-50.0, 50.0),
                        "tint": (now.tint + step.tint).clamp(-50.0, 50.0),
                        "saturation": (now.saturation * (1.0 + step.saturation)).clamp(0.0, 4.0),
                    })).map_err(|e| e.to_string())?;
                    let (plan, revision) = resolve_plan(ctx, &[op])?;
                    let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                    send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Grade To Target".into()))?;
                    done_rounds += 1;
                    m = measure_at(ctx, *mid, Some(6000))?;
                    // A round that leaves the shot worse off is not a
                    // step toward anything. The commonest case is a
                    // cast the primaries cannot reach — a tungsten shot
                    // against a daylight target — where the loop pushes
                    // exposure around for ever, crushes the blacks, and
                    // never moves the cast at all. Put the knobs back
                    // where they were and say why it stopped.
                    let after_round = colour_report(&m, &look, *mid);
                    if after_round.len() > fails.len() {
                        let op: klipp_document::ops::Op = serde_json::from_value(json!({
                            "op": "grade", "clip_id": id,
                            "exposure": now.exposure, "lift": now.lift, "gain": now.gain,
                            "temperature": now.temperature, "tint": now.tint, "saturation": now.saturation,
                        })).map_err(|e| e.to_string())?;
                        let (plan, revision) = resolve_plan(ctx, &[op])?;
                        let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                        send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some("Grade To Target".into()))?;
                        m = measure_at(ctx, *mid, Some(6000))?;
                        done_rounds -= 1;
                        stalled = Some(format!(
                            "stopped after {done_rounds} round(s): the next one made it worse ({} findings against {}), so it was put back. What is left is past what the primaries can reach — {}",
                            after_round.len(),
                            fails.len(),
                            if m.cast() >= 3.0 { "the cast needs the white balance set at the source, or a qualified secondary" } else { "try a secondary, or accept it" },
                        ));
                        break;
                    }
                    step = klipp_color::measure::primaries_toward(&m, &look);
                }
                let after = colour_report(&m, &look, *mid);
                let short: String = name.chars().take(28).collect();
                lines.push(format!(
                    "{short} at {mid:.2} s [{}]: {} before, {} after{}{}{}",
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
                per_shot.push(json!({ "clip_id": id, "secs": mid, "look": look.name, "before": before.iter().map(|f| json!({ "severity": f.0, "what": f.2, "fix": f.3 })).collect::<Vec<_>>(), "after": after.iter().map(|f| json!({ "severity": f.0, "what": f.2, "fix": f.3 })).collect::<Vec<_>>(), "rounds": done_rounds, "next_step": step, "stalled": stalled, "drift": drift, "frames_read": reads.len(), "lab": m }));
            }
            // A preset is a guess at what the piece wants; a reference
            // is what it was actually asked for.
            let briefed = ctx.sequence.read().ok().and_then(|s| s.brief.as_ref().and_then(|b| b.look.clone())).is_some();
            if !briefed {
                lines.push("These are held to a preset, because the brief names no look. adopt_look takes the numbers off a reference — a file you are matching, or this timeline's own best shot — and that is what a grade is normally briefed against.".to_string());
            }
            Ok(json!({ "text": lines.join("\n"), "shots": per_shot, "applied": apply, "revision": ctx.shared.revision(), "look_from_brief": briefed, "next": if apply { "check with color reads every shot again; match_scenes holds the shots to each other, which is what the eye sees at a cut; skin_balance for a face still off the band; a look (klipp_look) for style, after the grade." } else { "That is where every shot stands against the look and the first step each would take. Run it with apply true to grade them, one undo step a round." } }))
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
            // The draft, waited for: a review is the picture as rendered.
            let started = std::time::Instant::now();
            let job = loop {
                let jobs = ctx.shared.exports.lock().map_err(|_| "export status lock poisoned")?;
                let job = jobs.iter().find(|j| j.path == draft).cloned();
                drop(jobs);
                match job {
                    Some(j) if j.status == "complete" => break j,
                    Some(j) if j.status == "failed" || j.status == "cancelled" => return Err(format!("the draft render {}: {}", j.status, j.error.unwrap_or_default())),
                    _ => {}
                }
                if started.elapsed() > std::time::Duration::from_secs(600) {
                    return Err("the draft render did not finish within ten minutes; export_status says where it is".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            };
            let frames = frames.unwrap_or(((duration / 2.0).ceil() as u32).clamp(8, 48));
            let sheet = klipp_analysis::sheet::contact_sheet(std::path::Path::new("ffmpeg"), &draft, 0.0, duration, frames, cols.unwrap_or(6), 320, &sheet_out)?;
            let check = handle(ctx, ControlCommand::Check { color: true }, &mut None)?;
            Ok(json!({
                "draft": draft,
                "sheet": sheet,
                "render_secs": started.elapsed().as_secs_f64(),
                "loudness": job.loudness,
                "check": check,
                "text": format!("Draft rendered in {:.1} s at {w}x{h}; {frames} frames on the sheet at {}. {}", started.elapsed().as_secs_f64(), sheet_out.display(), check["text"].as_str().unwrap_or("")),
                "next": "Read the sheet for the shape of the cut, and the check for what to fix; frame_at for any frame that needs a closer look.",
            }))
        }

        ControlCommand::Narrate { text, at_secs, voice, download_models, apply, path } => {
            if in_batch {
                return Err("narrate synthesises and imports; send it on its own".into());
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err("narrate needs the words to say".into());
            }
            let piper = std::env::var("KLIPP_PIPER").ok().map(std::path::PathBuf::from).filter(|p| p.is_file()).or_else(|| which("piper")).or_else(|| which("piper-tts"))
                .ok_or("the piper program is not installed; on Arch: pacman -S piper-tts, elsewhere pip install piper-tts, or set KLIPP_PIPER to the program")?;
            let voice = voice.unwrap_or_else(|| "piper_en_us_lessac_medium".into());
            let (model, config) = voice_files(&voice, download_models)?;
            let out = match path {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    let dir = klipp_audio_search::download_dir().join("narration");
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    let stem: String = text.chars().filter(|c| c.is_alphanumeric() || *c == ' ').take(40).collect::<String>().trim().replace(' ', "-").to_lowercase();
                    dir.join(format!("{}-{:x}.wav", if stem.is_empty() { "narration".into() } else { stem }, seahash(&text)))
                }
            };
            let t = std::time::Instant::now();
            let mut child = std::process::Command::new(&piper)
                .arg("--model").arg(&model).arg("--config").arg(&config).arg("--output_file").arg(&out)
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                .spawn().map_err(|e| format!("could not run {}: {e}", piper.display()))?;
            {
                use std::io::Write as _;
                let mut stdin = child.stdin.take().ok_or("piper's stdin")?;
                stdin.write_all(text.as_bytes()).and_then(|_| stdin.write_all(b"\n")).map_err(|e| format!("writing to piper: {e}"))?;
            }
            let done = child.wait_with_output().map_err(|e| format!("piper: {e}"))?;
            if !done.status.success() || !out.is_file() {
                return Err(format!("piper failed ({}): {}", done.status, String::from_utf8_lossy(&done.stderr).trim()));
            }
            let mut v = handle(ctx, ControlCommand::ImportMedia { path: out.to_string_lossy().to_string(), analyze: true }, &mut None)?;
            let media_id = imported_id(ctx, &out).ok_or("the narration was made but did not reach the media pool")?;
            v["media_id"] = json!(media_id);
            v["path"] = json!(out);
            v["synth_secs"] = json!(t.elapsed().as_secs_f64());
            if apply {
                use klipp_document::ops::{Anchor, ClipRef, Op, TrackRef};
                let at = at_secs.unwrap_or(0.0).max(0.0);
                let ops: Vec<Op> = {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    let mut ops = Vec::new();
                    let track = match seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio && t.name.eq_ignore_ascii_case("voice")) {
                        Some(ti) => ti,
                        None => {
                            ops.push(Op::AddTrack { kind: "audio".into(), name: Some("Voice".into()) });
                            seq.tracks.len()
                        }
                    };
                    ops.push(Op::Place { media_id: klipp_core::MediaId(media_id.parse().map_err(|e| format!("media id: {e}"))?), track: TrackRef::Index(track), at: Anchor::Secs(at), source_in: None, source_out: None, insert: false, with_audio: true });
                    ops.push(Op::Role { clip_id: ClipRef::At { track, secs: at + 0.05 }, role: "dialogue".into() });
                    ops
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
                    let dir = klipp_audio_search::download_dir().parent().map(|p| p.join("recordings")).unwrap_or_else(std::env::temp_dir);
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
            let mut v = send(if analyze { AppCommand::ImportMedia { path: p.clone() } } else { AppCommand::ImportMediaWithAnalysis { path: p.clone(), analyze } })?;
            // The id it just made, waited for. Without this the first
            // call answers with no media_id at all and only a *second*
            // one finds the path in the pool — so an agent importing a
            // batch gets a list of nothing and has to ask again for
            // every file.
            match imported_id(ctx, &p) {
                Some(id) => {
                    v["media_id"] = json!(id);
                }
                None => {
                    v["note"] = json!("the import was sent but the item has not reached the pool yet; list_media will show it");
                }
            }
            v["analysis_requested"] = json!(analyze);
            if !analyze {
                v["next"] = json!("Imported without analysis. Call analyze with this media_id and wait true when results are needed.");
                return Ok(v);
            }
            v["next"] = json!("Imported. Analysis runs on import; set the brief first (set_brief) so the transcript gets the cast's names and the spellings. Watch analysis_status, then read the transcript and shots.");
            Ok(v)
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
            Ok(json!({
                "clip_id": c,
                "is_identity": clip.node_graph.is_identity(),
                "graph": serde_json::to_value(&clip.node_graph)
                    .map_err(|e| format!("serialising graph: {e}"))?,
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
            let mut v = json!({
                "path": out.to_string_lossy(),
                "width": width,
                "height": height,
                "generation": generation,
            });
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
            Ok(v)
        }

        ControlCommand::Apply { ops, label, detail } => {
            if in_batch {
                return Err("apply is its own batch; send it on its own".into());
            }
            let (plan, revision) = resolve_plan(ctx, &ops)?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), label)?;
            v["text"] = json!(if detail { plan.text() } else { plan.text_folded() });
            if detail {
                v["steps"] = json!(plan.report());
            }
            v["duration_secs"] = json!(plan.duration_secs);
            if let Some(next) = next_after_ops(&ops) {
                v["next"] = json!(next);
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
            let op = Op::Move { clip_id: ClipRef::Id(tgt_id), to: Anchor::Secs(new_in) };
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(format!("sync {tgt_name}")))?;
            v["text"] = json!(format!("Aligned {tgt_name} to the reference: offset {offset_secs:+.3} s, match {:.0}%. Moved it to {new_in:.3} s on the timeline.", confidence * 100.0));
            v["offset_secs"] = json!(offset_secs);
            v["confidence"] = json!(confidence);
            v["next"] = json!("Play across the join to confirm the sound and picture line up. If it drifts over a long take, the two clocks differ — you may need a second sync point.");
            Ok(v)
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

        ControlCommand::Look { clip_id: c, name, opacity, remove } => {
            if in_batch {
                return Err("look applies a grade; send it on its own, not in a batch".into());
            }
            let target_id = clip_id(&c)?;
            use klipp_document::ops::{ClipRef, Op};
            let op = if remove {
                Op::Look { clip_id: ClipRef::Id(target_id), lut_path: String::new(), name: None, opacity: None, remove: true }
            } else {
                let name = name.ok_or("give a look name (see looks), or remove to clear")?;
                let (path, display) = resolve_look(&name)?;
                Op::Look { clip_id: ClipRef::Id(target_id), lut_path: path.to_string_lossy().to_string(), name: Some(display), opacity, remove: false }
            };
            let (plan, revision) = resolve_plan(ctx, std::slice::from_ref(&op))?;
            let edits: Vec<klipp_document::history::EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
            let mut v = send_full_envelope(ctx, Vec::new(), edits, true, Some(revision), Some(plan.steps.first().map(|s| s.what.clone()).unwrap_or_else(|| "look".into())))?;
            v["text"] = json!(plan.text_folded());
            v["next"] = json!("Look on. See it with frame_at or screenshot; check color reads the graded frame. Fade it with opacity, or remove to clear.");
            Ok(v)
        }

        ControlCommand::MatchGrade { clip_id: c, reference_clip_id: r } => {
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
                let mid = |cl: &klipp_timeline::Clip| (cl.timeline_in.to_secs_f64() + cl.timeline_out.to_secs_f64()) / 2.0;
                let name = pool.get(&tc.media_id().unwrap_or(klipp_core::MediaId(uuid::Uuid::nil()))).map(|m| m.filename.clone()).unwrap_or_else(|| "the clip".into());
                (mid(&tc), mid(&rc), name)
            };
            let (td, tw, th) = grab_frame_rgba(ctx, t_mid, Some(4000))?;
            let target_stats = frame_stats(&td, tw, th, t_mid);
            let (rd, rw, rh) = grab_frame_rgba(ctx, r_mid, Some(4000))?;
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
                clip_id: ClipRef::Id(target_id),
                exposure: knobs.exposure, contrast: None, saturation: None,
                temperature: knobs.temperature, tint: knobs.tint,
                lift: knobs.lift, gamma: None, gain: knobs.gain,
                highlights: None, shadows: None, vibrance: None, reset: false,
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
                    fb.latest_presented().map(|t| t.to_secs_f64()).unwrap_or(0.0)
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

        ControlCommand::SearchMusic { query, limit, commercial_only, instrumental } => {
            if in_batch {
                return read_in_batch("search_music");
            }
            if !klipp_audio_search::music_available() {
                return Err("music search is not set up: put jamendo_client_id in ~/.config/klipp/keys.toml (or set KLIPP_JAMENDO_CLIENT_ID)".into());
            }
            let tracks = klipp_audio_search::search_music_filtered(&query, limit.unwrap_or(12), klipp_audio_search::MusicFilter { commercial_only, instrumental })?;
            let mut text = format!("{} royalty-free music results for \"{query}\" (Jamendo):\n", tracks.len());
            for t in &tracks {
                text.push_str(&format!("  {} — {}  ({}, {:.0}s){}\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }));
            }
            let results: Vec<serde_json::Value> = tracks.iter().map(|t| json!({
                "title": t.title, "artist": t.artist, "track_id": t.id,
                "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                "commercial_ok": t.commercial_ok, "preview_url": t.preview_url, "download_url": t.download_url,
                "page_url": t.page_url, "tags": t.tags,
            })).collect();
            Ok(json!({ "text": text, "results": results, "next": "Listen to a track's preview_url before you commit. Add one with add_music (copy its title, artist, download_url, license, license_url, page_url). Honour the licence: non-commercial tracks stay out of commercial work, and the credit is kept for you." }))
        }

        ControlCommand::SearchSfx { query, limit } => {
            if in_batch {
                return read_in_batch("search_sfx");
            }
            if !klipp_audio_search::sfx_available() {
                return Err("sound-effect search is not set up: put freesound_token in ~/.config/klipp/keys.toml (or set KLIPP_FREESOUND_TOKEN)".into());
            }
            let tracks = klipp_audio_search::search_sfx(&query, limit.unwrap_or(12))?;
            let mut text = format!("{} royalty-free sound effects for \"{query}\" (Freesound):\n", tracks.len());
            for t in &tracks {
                text.push_str(&format!("  {} — {}  ({}, {:.1}s){}\n", t.title, t.artist, t.license, t.duration_secs, if t.commercial_ok { "" } else { " [non-commercial]" }));
            }
            let results: Vec<serde_json::Value> = tracks.iter().map(|t| json!({
                "title": t.title, "artist": t.artist, "track_id": t.id,
                "duration_secs": t.duration_secs, "license": t.license, "license_url": t.license_url,
                "commercial_ok": t.commercial_ok, "preview_url": t.preview_url, "download_url": t.download_url,
                "page_url": t.page_url, "tags": t.tags,
            })).collect();
            Ok(json!({ "text": text, "results": results, "next": "Preview a sound's preview_url, then add it with add_music (it takes a sound effect just the same: pass title, artist, preview_url, license, license_url, page_url). The credit is kept; honour the licence." }))
        }

        ControlCommand::AddMusic { title, artist, track_id, download_url, preview_url, license, license_url, page_url } => {
            if in_batch {
                return Err("add_music downloads and imports a file; send it on its own, not in a batch".into());
            }
            let track = klipp_audio_search::Track {
                source: klipp_audio_search::Source::JamendoMusic,
                id: track_id, title: title.clone(), artist: artist.clone(),
                duration_secs: 0.0, license: license.clone(), license_url,
                commercial_ok: true, preview_url, download_url, page_url, image_url: String::new(), tags: Vec::new(),
            };
            let path = klipp_audio_search::download_track(&track, &klipp_audio_search::download_dir())?;
            let mut v = handle(ctx, ControlCommand::ImportMedia { path: path.to_string_lossy().to_string(), analyze: true }, &mut None)?;
            v["text"] = json!(format!("Added \"{title}\" by {artist} ({license}) to the media pool. Place it on an audio track, mark the track Music, and duck it under speech.", license = if license.is_empty() { "licence with the file".to_string() } else { license }));
            v["attribution"] = json!(track.attribution());
            v["next"] = json!("Place it on an audio track (append_clip), set the track's role to Music so the loudness check treats it right, and duck it under any speech. The credit is saved beside the file for your export notes.");
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
                    let faces: Vec<[f32; 4]> = report.framing.as_ref().and_then(|f| f.people_at(t, 1.5)).map(|s| s.people.iter().map(|p| klipp_color::measure::head_region([p.0, p.1, p.2, p.3])).collect()).unwrap_or_default();
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
                (timeline_text(&seq, &pool, None, ctx.shared.revision()), clips)
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
            v["steps_resolved"] = json!(plan.steps.len());
            v["duration_secs"] = json!(plan.duration_secs);
            v["ops"] = json!(ops);
            if detail {
                v["steps"] = json!(plan.report());
            }
            Ok(v)
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
            let text = brief.text();
            let new_spellings = brief.spellings();
            let edit = klipp_document::history::EditCommand::SetBrief { old: old.clone().map(Box::new), new: Some(Box::new(brief)) };
            let respelt = old.as_ref().map(|b| b.spellings()).unwrap_or_default() != new_spellings;
            let mut v = send_full_envelope(ctx, Vec::new(), vec![edit], true, None, Some("Set Brief".into()))?;
            v["text"] = json!(text);
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
                // One time per picture clip on the top video track: its
                // middle, where the grade is fairly judged.
                let times: Vec<(f64, (f64, f64))> = if color {
                    seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
                        .map(|t| t.clips.iter().filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }))
                            .map(|c| {
                                let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
                                ((a + b) / 2.0, (a, b))
                            }).take(24).collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                // Each distinct piece of media that carries sound, with a
                // time it plays at — for reading its spectral faults off
                // the analysis (no rendering, so this is always on).
                let mut seen = std::collections::HashSet::new();
                let mut audio_media: Vec<(std::path::PathBuf, String, f64)> = Vec::new();
                for t in &seq.tracks {
                    for c in t.clips.iter().filter(|c| c.enabled) {
                        if let Some(mid) = c.media_id() {
                            if let Some(item) = pool.get(&mid) {
                                if item.has_audio && seen.insert(mid) {
                                    audio_media.push((item.path.clone(), item.filename.clone(), (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0));
                                }
                            }
                        }
                    }
                }
                (report, times, audio_media)
            };
            // Every shot read in Lab against the brief's look (or the
            // neutral one, or the screen recording's), faces and all.
            let mut color_findings: Vec<(String, f64, String, String)> = Vec::new();
            let mut shots_on_target = 0usize;
            for (t, span) in &color_times {
                let Ok((reads, drift)) = measure_clip(ctx, span.0, span.1, Some(4000)) else { continue };
                let (_, m) = median_measure(&reads);
                let look = look_for(ctx, *t);
                let mut found = colour_report(&m, &look, *t);
                // The chain's own order, which no measurement can see:
                // a balance behind a look never settles, however many
                // rounds the loop runs.
                {
                    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
                    if let Some(c) = seq.tracks.iter().flat_map(|tr| tr.clips.iter()).find(|c| c.timeline_in.to_secs_f64() <= *t && c.timeline_out.to_secs_f64() > *t && matches!(c.kind, klipp_timeline::ClipKind::Media { .. })) {
                        for fault in c.node_graph.chain_out_of_order() {
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
            // Sound faults: wind, rumble, hum, hiss, from the analysis bands.
            let mut audio_findings: Vec<(f64, String, String, String)> = Vec::new();
            for (path, name, at) in &audio_media {
                // A library track or effect is what it is: its low end is
                // not a hum to clean.
                if klipp_analysis::is_licensed_audio(path) {
                    continue;
                }
                if let Some(au) = klipp_analysis::report::MediaAnalysis::load(path).audio {
                    for (what, fix) in au.faults() {
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
                text.push_str(&format!("\n\nColour (measured in Lab on {} shot{}, {shots_on_target} on target):", color_times.len(), if color_times.len() == 1 { "" } else { "s" }));
                if color_findings.is_empty() {
                    text.push_str(" nothing amiss.");
                } else {
                    for f in &color_findings {
                        text.push_str(&format!("\n[{}] {:.2} s  {}. {}", f.0, f.1, f.2, f.3));
                    }
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
            Ok(json!({ "text": text, "errors": errors, "warnings": warnings, "findings": report.findings, "color_findings": findings_json, "shots_on_target": shots_on_target, "sound_findings": audio_json, "duration_secs": report.duration_secs, "revision": ctx.shared.revision(), "next": next }))
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

        ControlCommand::Export { path, width, height, fps, crf, bit_depth, target_lufs, true_peak_dbtp, burn_captions } => {
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
                "size": settings.resolution.map(|r| format!("{}x{}", r.width, r.height)),
            });
            let burnt = said["captions_and_titles_burnt_in"].as_bool().unwrap_or(true);
            let mut v = send(AppCommand::ExportSequence { settings })?;
            v["export"] = said;
            v["next"] = json!(format!(
                "Rendering. Read export_status for the job_id, then call it with that job_id and wait true. A finished job may have failed or been cancelled: only succeeded true means the file is ready.{}",
                if burnt { "" } else { " Captions are not burnt in: write export_subtitles for a sidecar SRT." }
            ));
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

        ControlCommand::FrameAt { secs, path, max_width, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_at");
            }
            // One at a time: there is one playhead, and two grabs at once
            // seek it out from under each other and the second fails.
            static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
            let held = hold_transport(ctx)?;
            let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
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
}
