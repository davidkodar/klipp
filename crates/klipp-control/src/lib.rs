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
/// Seek to `secs`, wait for the frame there, read its RGBA bytes, and
/// seek back — the picture as the viewer shows it, for measuring. Shares
/// the one-grab lock and the wait with frame_at.
fn grab_frame_rgba(ctx: &ControlContext, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
    let (was_at, fps) = {
        let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
        let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
        (fb.latest_presented(), seq.fps)
    };
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
            if let Some(t) = was_at {
                let _ = send_envelope(ctx, vec![AppCommand::SeekTo(t)], false);
            }
            return Err(format!("no frame at {secs:.3} s within {} ms; give a longer timeout_ms", wait.as_millis()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (data, width, height) = {
        let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
        let (d, w, h, _) = fb.latest_frame();
        (d.to_vec(), w, h)
    };
    if let Some(t) = was_at {
        let _ = send_envelope(ctx, vec![AppCommand::SeekTo(t)], false);
    }
    let expected = (width as usize) * (height as usize) * 4;
    if width == 0 || height == 0 || data.len() < expected {
        return Err("the viewer has no frame to measure yet".into());
    }
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
}

/// What to do after a batch of ops was applied, from the ops in it: the
/// reply points at the next step so a session follows a trail rather
/// than recalling a recipe.
fn next_after_ops(ops: &[klipp_document::ops::Op]) -> Option<&'static str> {
    use klipp_document::ops::Op;
    let has = |f: &dyn Fn(&Op) -> bool| ops.iter().any(f);
    if has(&|o| matches!(o, Op::Place { with_audio: true, .. })) && ops.len() >= 2 {
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

        ControlCommand::Analyze { media_id, download_models } => {
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
            let statuses: Vec<klipp_analysis::service::Status> = items.into_iter().map(klipp_analysis::service::request).collect();
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

        ControlCommand::AnalysisStatus => {
            if in_batch {
                return read_in_batch("analysis_status");
            }
            // Which models are there says what analysis can be had at all:
            // with no speech model, a transcript is not late, it is not coming.
            Ok(json!({
                "items": klipp_analysis::service::all(),
                "models": {
                    "speech": klipp_analysis::transcript::installed_model().map(|(id, _)| id),
                    "detection": klipp_analysis::framing::model_path().map(|(id, _)| id),
                },
                "note": "current is the analyser at work and current_for_secs how long it has been; a transcript takes about a third of the file's length",
            }))
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
            }))
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

        ControlCommand::ImportMedia { path } => {
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
            let mut v = send(AppCommand::ImportMedia { path: p })?;
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

        ControlCommand::SearchMusic { query, limit } => {
            if in_batch {
                return read_in_batch("search_music");
            }
            if !klipp_audio_search::music_available() {
                return Err("music search is not set up: put jamendo_client_id in ~/.config/klipp/keys.toml (or set KLIPP_JAMENDO_CLIENT_ID)".into());
            }
            let tracks = klipp_audio_search::search_music(&query, limit.unwrap_or(12))?;
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
            let mut v = handle(ctx, ControlCommand::ImportMedia { path: path.to_string_lossy().to_string() }, &mut None)?;
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
            let beats: Vec<f64> = b.beat_times.iter().take(500).copied().collect();
            Ok(json!({ "text": text, "media_id": media_id, "bpm": b.bpm, "confidence": b.confidence, "beat_times": beats, "next": "Place or trim spoken and B-roll cuts to a beat time for a musical rhythm. style_profile reads a whole reference at once." }))
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
            for i in 0..n {
                let t = (i as f64 + 0.5) / n as f64 * dur;
                if let Ok((data, w, h)) = decode_frame_rgba(&path, t) {
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
                json!({ "black_ire": black.round(), "mid_ire": mid.round(), "white_ire": white.round(), "contrast_ire": contrast.round(), "saturation_pct": sat.round(), "warmth": (warm.round()), "summary": format!("{tone}, {punch}, {col}") })
            } else {
                json!(null)
            };
            let motion = report.motion.as_ref().map(|m| json!({ "class": klipp_analysis::motion::class_for(m.mean), "mean": m.mean }));

            // Guidance: turn the numbers into how to build the same feel.
            let pace = cuts["pace"].as_str().unwrap_or("medium");
            let mean_shot = cuts["mean_shot_secs"].as_f64().unwrap_or(0.0);
            let mut guide = format!("Cut {pace}: aim for shots around {mean_shot:.1} s. ");
            if beats.confidence > 0.3 {
                let onb = cuts["cut_on_beat_ratio"].as_f64().unwrap_or(0.0);
                if onb > 0.4 {
                    guide.push_str(&format!("It cuts on the beat ({:.0}% of cuts) at {:.0} BPM — place your cuts on beat times (klipp_beats on your music). ", onb * 100.0, beats.bpm));
                } else {
                    guide.push_str(&format!("The music runs at {:.0} BPM; lean cuts toward the beat. ", beats.bpm));
                }
            }
            if let Some(l) = look.as_object() {
                guide.push_str(&format!("Grade toward the look ({}): blacks ~{} IRE, whites ~{} IRE, saturation ~{}%. Use match_grade against a frame of the reference, or look for a LUT. ", l.get("summary").and_then(|v| v.as_str()).unwrap_or(""), l.get("black_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("white_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("saturation_pct").and_then(|v| v.as_f64()).unwrap_or(0.0)));
            }
            guide.push_str("Match the energy with your own or licensed music, not the reference's track.");

            let text = format!(
                "Style of {filename}:\ncuts — {} shots, mean {:.1} s, {:.0} per minute ({pace}){}\nlook — {}\nmotion — {}\n\n{guide}",
                cuts["shot_count"].as_i64().unwrap_or(0),
                mean_shot,
                cuts["cuts_per_min"].as_f64().unwrap_or(0.0),
                if beats.confidence > 0.3 { format!("; {:.0} BPM", beats.bpm) } else { String::new() },
                look.get("summary").and_then(|v| v.as_str()).unwrap_or("not measured"),
                motion.as_ref().and_then(|m| m.get("class")).and_then(|v| v.as_str()).unwrap_or("unknown"),
            );
            Ok(json!({
                "text": text, "media_id": media_id, "filename": filename,
                "cuts": cuts,
                "tempo": { "bpm": beats.bpm, "confidence": beats.confidence },
                "look": look,
                "motion": motion,
                "guidance": guide,
                "next": "Build the edit in this feel from your own footage: selects at the target shot length, cuts on the beat (klipp_beats on your music), grade toward the look, then transitions and effects to taste. Never the reference's actual music or footage.",
            }))
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
            let brief = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.brief.clone();
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
            } else if clips == 0 {
                "The footage is analysed. Read a transcript (with words) to choose the selects, then place them; find_shots with no people for B-roll over the joins."
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
                    .filter_map(|m| klipp_analysis::report::MediaAnalysis::load(&m.path).transcript.map(|t| (m.filename.clone(), t.vocabulary)))
                    .filter(|(_, had)| klipp_analysis::transcript::lacks_spellings(had, &new_spellings))
                    .map(|(f, _)| f)
                    .collect();
                v["note"] = json!(if lacking.is_empty() {
                    "the transcripts made so far already carry these spellings; a file analysed from now on gets them too".to_string()
                } else {
                    format!("the spellings changed and these transcripts were made without them: {}; run analyze to have them transcribed again before captions or a transcript search", lacking.join(", "))
                });
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
                let times: Vec<f64> = if color {
                    seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
                        .map(|t| t.clips.iter().filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }))
                            .map(|c| (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0).take(24).collect())
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
            let mut color_findings = Vec::new();
            for t in &color_times {
                if let Ok((data, w, h)) = grab_frame_rgba(ctx, *t, Some(4000)) {
                    color_findings.extend(color_faults(&frame_stats(&data, w, h, *t), *t));
                }
            }
            // Sound faults: wind, rumble, hum, hiss, from the analysis bands.
            let mut audio_findings: Vec<(f64, String, String, String)> = Vec::new();
            for (path, name, at) in &audio_media {
                if let Some(au) = klipp_analysis::report::MediaAnalysis::load(path).audio {
                    for (what, fix) in au.faults() {
                        audio_findings.push((*at, name.clone(), what, fix));
                    }
                }
            }

            let next = if report.errors > 0 {
                "Fix the errors before you deliver — each finding says what and where. Then check again."
            } else if report.warnings > 0 {
                "No errors. Decide each warning: fix it, or keep it on purpose and be able to say why. Then export."
            } else {
                "Clean. Export at the brief's loudness, poll export_status, then save_project."
            };
            let mut text = report.text();
            if color {
                text.push_str(&format!("\n\nColour (measured on {} shot{}):", color_times.len(), if color_times.len() == 1 { "" } else { "s" }));
                if color_findings.is_empty() {
                    text.push_str(" nothing amiss.");
                } else {
                    for f in &color_findings {
                        text.push_str(&format!("\n[warning] {:.2} s  {}. {}", f.0, f.1, f.2));
                    }
                }
            }
            if !audio_findings.is_empty() {
                text.push_str("\n\nSound:");
                for f in &audio_findings {
                    text.push_str(&format!("\n[warning] {}: {}. {}", f.1, f.2, f.3));
                }
            }
            let findings_json: Vec<serde_json::Value> = color_findings.iter().map(|f| json!({ "severity": "warning", "rule": "color", "at_secs": f.0, "what": f.1, "hint": f.2 })).collect();
            let audio_json: Vec<serde_json::Value> = audio_findings.iter().map(|f| json!({ "severity": "warning", "rule": "sound", "at_secs": f.0, "media": f.1, "what": f.2, "hint": f.3 })).collect();
            Ok(json!({ "text": text, "errors": report.errors, "warnings": report.warnings + color_findings.len() + audio_findings.len(), "findings": report.findings, "color_findings": findings_json, "sound_findings": audio_json, "duration_secs": report.duration_secs, "revision": ctx.shared.revision(), "next": next }))
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

        ControlCommand::Export { path, width, height, fps, crf, target_lufs, burn_captions } => {
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
            if let Some(burn) = burn_captions {
                settings.burn_in_subtitles = burn;
            }
            // Said, because it cannot be seen from the reply otherwise and
            // decides whether a sidecar file is needed.
            let said = json!({
                "captions_and_titles_burnt_in": settings.burn_in_subtitles,
                "target_lufs": settings.target_lufs,
                "size": settings.resolution.map(|r| format!("{}x{}", r.width, r.height)),
            });
            let burnt = said["captions_and_titles_burnt_in"].as_bool().unwrap_or(true);
            let mut v = send(AppCommand::ExportSequence { settings })?;
            v["export"] = said;
            v["next"] = json!(format!(
                "Rendering. Poll export_status until it is complete — it carries the frames a second and an ETA — then the file is on disk and its measured loudness is reported.{}",
                if burnt { "" } else { " Captions are not burnt in: write export_subtitles for a sidecar SRT." }
            ));
            Ok(v)
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

        ControlCommand::FrameStats { secs, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_stats");
            }
            let (data, width, height) = grab_frame_rgba(ctx, secs, timeout_ms)?;
            Ok(frame_stats(&data, width, height, secs))
        }

        ControlCommand::FrameAt { secs, path, max_width, timeout_ms } => {
            if in_batch {
                return read_in_batch("frame_at");
            }
            // One at a time: there is one playhead, and two grabs at once
            // seek it out from under each other and the second fails.
            static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
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
