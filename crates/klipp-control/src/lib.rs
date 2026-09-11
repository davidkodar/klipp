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

use klipp_core::AppCommand;
use protocol::{ControlCommand, Request, Response};
use serde_json::json;

/// Everything the server needs to read state and send commands. Built
/// in `main.rs` from the same handles the UI and engine already share.
#[derive(Clone)]
pub struct ControlContext {
    /// Into the UI's command router, *not* the engine's channel — see the
    /// module docs for why the difference matters.
    pub commands: crossbeam_channel::Sender<AppCommand>,
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
                match handle(&ctx, req.command) {
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

fn handle(ctx: &ControlContext, cmd: ControlCommand) -> Result<serde_json::Value, String> {
    let send = |c: AppCommand| -> Result<serde_json::Value, String> {
        // Read the generation *before* queueing, so any frame rendered in
        // response to this command is guaranteed to be newer than it.
        let generation = ctx
            .frame_buffer
            .lock()
            .map(|fb| fb.latest_generation())
            .unwrap_or(0);
        ctx.commands
            .send(c)
            .map_err(|e| format!("app command channel closed: {e}"))?;
        (ctx.wake_ui)();
        // Commands are queued for the UI's next frame, not executed
        // inline, so "accepted" is the honest answer. To see the effect,
        // screenshot with `min_generation: generation + 1`.
        Ok(json!({ "accepted": true, "generation": generation }))
    };

    match cmd {
        ControlCommand::Ping => Ok(json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        })),

        ControlCommand::Status => {
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            let perf = ctx.perf_ring.lock().ok().and_then(|r| r.latest().cloned());
            Ok(json!({
                "tracks": seq.tracks.len(),
                "clips": seq.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
                "fps": seq.fps.as_f64(),
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
            let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
            let pool = ctx.media_pool.read().map_err(|_| "media pool lock poisoned")?;
            let tracks: Vec<serde_json::Value> = seq
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    json!({
                        "index": i,
                        "opacity": t.opacity,
                        "clips": t.clips.iter().map(|c| json!({
                            "clip_id": c.id.0.to_string(),
                            "enabled": c.enabled,
                            "timeline_in_secs": c.timeline_in.to_secs_f64(),
                            "timeline_out_secs": c.timeline_out.to_secs_f64(),
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
                                })),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(json!({ "tracks": tracks }))
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
            send(AppCommand::RemoveClip { clip_id: clip_id(&c)? })
        }
        ControlCommand::SelectClip { clip_id: c } => {
            let id = match c {
                Some(s) => Some(clip_id(&s)?),
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
            send(AppCommand::AddNode { clip_id: clip_id(&c)?, kind, position: (x, y) })
        }
        ControlCommand::RemoveNode { clip_id: c, node_id } => send(AppCommand::RemoveNode {
            clip_id: clip_id(&c)?,
            node_id: parse_uuid(&node_id, "node_id")?,
        }),
        ControlCommand::ConnectNodes { clip_id: c, from, from_port, to, to_port } => {
            send(AppCommand::ConnectNodes {
                clip_id: clip_id(&c)?,
                from: parse_uuid(&from, "from")?,
                from_port,
                to: parse_uuid(&to, "to")?,
                to_port,
            })
        }
        ControlCommand::SetNodeParam { clip_id: c, node_id, param, value } => {
            send(AppCommand::SetNodeParam {
                clip_id: clip_id(&c)?,
                node_id: parse_uuid(&node_id, "node_id")?,
                param,
                value,
            })
        }
        ControlCommand::ToggleNodeEnabled { clip_id: c, node_id } => {
            send(AppCommand::ToggleNodeEnabled {
                clip_id: clip_id(&c)?,
                node_id: parse_uuid(&node_id, "node_id")?,
            })
        }

        ControlCommand::Screenshot { path, max_width, min_generation, timeout_ms } => {
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(timeout_ms.unwrap_or(3000));
            let (data, width, height, generation) = loop {
                {
                    let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
                    let (d, w, h, g) = fb.latest_frame();
                    if min_generation.map_or(true, |m| g >= m) {
                        break (d.to_vec(), w, h, g);
                    }
                    if std::time::Instant::now() >= deadline {
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
            if width == 0 || height == 0 || data.is_empty() {
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

        ControlCommand::Perf => {
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
    let mut writer = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
    writer.write_image_data(rgba).map_err(|e| format!("png data: {e}"))?;
    Ok(())
}
