//! Model Context Protocol server for Klipp.
//!
//! Speaks MCP (JSON-RPC 2.0, one JSON object per line on stdio) to a
//! client such as Claude Code, and forwards each tool call to a running
//! Klipp over its control socket.
//!
//! What that buys: an agent can read the timeline, drive playback, apply
//! a grade and then *look at the resulting frame*, instead of asking a
//! human to click and screenshot.
//!
//! Register with Claude Code:
//!   claude mcp add klipp -- /path/to/klipp-mcp
//!
//! The socket path follows `KLIPP_CONTROL_SOCKET`, else
//! `$XDG_RUNTIME_DIR/klipp-control.sock`. Klipp must be running; every
//! tool answers with a clear "is Klipp running?" error if it is not,
//! since that is the overwhelmingly common cause.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";
/// Inline a screenshot in the reply only when it is small enough to be
/// worth a client's context window. Bigger ones come back as a path.
const INLINE_IMAGE_MAX_BYTES: usize = 1_000_000;

/// Generation of the viewer frame when the most recent write was queued,
/// or `u64::MAX` when no write is outstanding.
///
/// Writes are asynchronous — a tool call returns as soon as the command
/// is queued, well before a frame reflecting it exists. Without this, a
/// screenshot taken straight after a grade change captures the *old*
/// frame, and a "change then look" check silently compares a frame with
/// itself. That is exactly what happened the first time this was used.
static PENDING_WRITE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// Tools that change what the viewer should show. Any of them arms the
/// next screenshot to wait for a fresh frame.
const WRITE_TOOLS: &[&str] = &[
    "klipp_play", "klipp_pause", "klipp_stop", "klipp_seek",
    "klipp_append_clip", "klipp_remove_clip", "klipp_attach_audio", "klipp_set_append_audio", "klipp_select_clip",
    "klipp_add_node", "klipp_remove_node", "klipp_connect_nodes",
    "klipp_set_node_param", "klipp_toggle_node", "klipp_set_log_format",
    "klipp_undo", "klipp_redo", "klipp_batch", "klipp_export", "klipp_export_subtitles", "klipp_apply",
    "klipp_open_project",
];

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Parse error, JSON-RPC code -32700. No id available.
                let _ = writeln!(
                    stdout,
                    "{}",
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") }
                    })
                );
                let _ = stdout.flush();
                continue;
            }
        };

        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned();

        // Notifications carry no id and must get no response at all.
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "klipp", "version": env!("CARGO_PKG_VERSION") },
                }
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions() },
            }),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(name, &args) {
                    Ok(content) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": content, "isError": false },
                    }),
                    // A failed tool call is a *result* with isError, not a
                    // JSON-RPC error: the model should see the message and
                    // be able to act on it.
                    Err(e) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": e }],
                            "isError": true,
                        },
                    }),
                }
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") },
            }),
        };

        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}

/// Send one control request and read one reply.
///
/// A fresh connection per call. Klipp's control server is one thread per
/// connection and calls are infrequent and human-paced, so holding a
/// persistent socket would buy nothing and would need reconnect handling
/// on every Klipp restart — which, during development, is constant.
fn control_call(command: Value) -> Result<Value, String> {
    let path = klipp_control::protocol::socket_path();
    let stream = UnixStream::connect(&path).map_err(|e| {
        format!(
            "cannot reach Klipp on {}: {e}. Is Klipp running? \
             (the socket appears once the app has started; \
             KLIPP_CONTROL=0 disables it entirely)",
            path.display(),
        )
    })?;
    let mut writer = stream.try_clone().map_err(|e| format!("socket clone failed: {e}"))?;
    let mut reader = BufReader::new(stream);

    let mut payload = command;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("id".into(), json!(1));
    }
    let mut text = payload.to_string();
    text.push('\n');
    writer
        .write_all(text.as_bytes())
        .map_err(|e| format!("sending to Klipp failed: {e}"))?;
    writer.flush().ok();

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("reading from Klipp failed: {e}"))?;
    if line.trim().is_empty() {
        return Err("Klipp closed the connection without replying".into());
    }
    let reply: Value =
        serde_json::from_str(&line).map_err(|e| format!("bad reply from Klipp: {e}"))?;
    if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(reply.get("result").cloned().unwrap_or(json!({})))
    } else {
        Err(reply
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string())
    }
}

fn text_content(v: &Value) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    })]
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

/// The socket command a tool call is. Shared by a single call and by the
/// calls inside a batch.
fn tool_to_command(name: &str, args: &Value) -> Result<Value, String> {
    Ok(match name {
        "klipp_status" => json!({ "method": "status" }),
        "klipp_play" => json!({ "method": "play" }),
        "klipp_pause" => json!({ "method": "pause" }),
        "klipp_stop" => json!({ "method": "stop" }),
        "klipp_seek" => json!({
            "method": "seek",
            "secs": args.get("secs").and_then(|v| v.as_f64())
                .ok_or("missing required argument: secs")?,
        }),
        "klipp_list_clips" => json!({ "method": "list_clips" }),
        "klipp_list_media" => json!({ "method": "list_media" }),
        "klipp_import_media" => json!({
            "method": "import_media", "path": arg_str(args, "path")?,
        }),
        "klipp_append_clip" => {
            let mut v = json!({
                "method": "append_clip", "media_id": arg_str(args, "media_id")?,
            });
            if let Some(t) = args.get("track").and_then(|t| t.as_u64()) {
                v["track"] = json!(t as usize);
            }
            v
        }
        "klipp_remove_clip" => json!({
            "method": "remove_clip", "clip_id": arg_str(args, "clip_id")?,
        }),
        "klipp_attach_audio" => json!({
            "method": "attach_audio", "clip_id": arg_str(args, "clip_id")?,
        }),
        "klipp_set_append_audio" => json!({
            "method": "set_append_audio",
            "enabled": args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        }),
        "klipp_select_clip" => json!({
            "method": "select_clip",
            "clip_id": args.get("clip_id").and_then(|v| v.as_str()),
        }),
        "klipp_get_grade" => json!({
            "method": "get_grade", "clip_id": arg_str(args, "clip_id")?,
        }),
        "klipp_add_node" => json!({
            "method": "add_node",
            "clip_id": arg_str(args, "clip_id")?,
            "kind": arg_str(args, "kind")?,
            "x": args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            "y": args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        }),
        "klipp_remove_node" => json!({
            "method": "remove_node",
            "clip_id": arg_str(args, "clip_id")?,
            "node_id": arg_str(args, "node_id")?,
        }),
        "klipp_connect_nodes" => json!({
            "method": "connect_nodes",
            "clip_id": arg_str(args, "clip_id")?,
            "from": arg_str(args, "from")?,
            "from_port": args.get("from_port").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
            "to": arg_str(args, "to")?,
            "to_port": args.get("to_port").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
        }),
        "klipp_set_node_param" => json!({
            "method": "set_node_param",
            "clip_id": arg_str(args, "clip_id")?,
            "node_id": arg_str(args, "node_id")?,
            "param": arg_str(args, "param")?,
            "value": args.get("value").and_then(|v| v.as_f64())
                .ok_or("missing required argument: value")? as f32,
        }),
        "klipp_toggle_node" => json!({
            "method": "toggle_node_enabled",
            "clip_id": arg_str(args, "clip_id")?,
            "node_id": arg_str(args, "node_id")?,
        }),
        "klipp_set_log_format" => json!({
            "method": "set_log_format",
            "clip_id": arg_str(args, "clip_id")?,
            "format": arg_str(args, "format")?,
        }),
        "klipp_screenshot" => {
            let mut v = json!({ "method": "screenshot" });
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            // After a write, wait for the frame that reflects it unless
            // the caller opts out. A plain look with no write pending
            // captures immediately.
            let pending = PENDING_WRITE_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
            let wait = args
                .get("wait_for_new_frame")
                .and_then(|v| v.as_bool())
                .unwrap_or(pending != u64::MAX);
            if wait && pending != u64::MAX {
                v["min_generation"] = json!(pending.saturating_add(1));
            }
            if let Some(t) = args.get("timeout_ms").and_then(|v| v.as_u64()) {
                v["timeout_ms"] = json!(t);
            }
            // Default to something a model can actually look at rather
            // than a 33 MB 4K frame.
            v["max_width"] = json!(
                args.get("max_width").and_then(|v| v.as_u64()).unwrap_or(1280)
            );
            v
        }
        "klipp_perf" => json!({ "method": "perf" }),
        "klipp_log_tail" => {
            let mut v = json!({ "method": "log_tail" });
            if let Some(n) = args.get("lines").and_then(|v| v.as_u64()) {
                v["lines"] = json!(n as usize);
            }
            v
        }
        "klipp_timeline" => {
            let mut v = json!({ "method": "timeline" });
            if let Some(n) = args.get("max_chars").and_then(|v| v.as_u64()) {
                v["max_chars"] = json!(n as usize);
            }
            v
        }
        "klipp_analyze" => {
            let mut v = json!({ "method": "analyze" });
            if let Some(id) = args.get("media_id").and_then(|v| v.as_str()) {
                v["media_id"] = json!(id);
            }
            if let Some(d) = args.get("download_models").and_then(|v| v.as_bool()) {
                v["download_models"] = json!(d);
            }
            v
        }
        "klipp_search_transcripts" => {
            let mut v = json!({ "method": "search_transcripts", "query": arg_str(args, "query")? });
            if let Some(n) = args.get("max_hits").and_then(|v| v.as_u64()) {
                v["max_hits"] = json!(n as usize);
            }
            v
        }
        "klipp_analysis_status" => json!({ "method": "analysis_status" }),
        "klipp_media_analysis" => {
            let mut v = json!({ "method": "media_analysis", "media_id": arg_str(args, "media_id")? });
            if let Some(n) = args.get("max_chars").and_then(|v| v.as_u64()) {
                v["max_chars"] = json!(n as usize);
            }
            v
        }
        "klipp_find_shots" => {
            let mut v = json!({ "method": "find_shots" });
            for key in ["framing", "object", "motion"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_str()) {
                    v[key] = json!(x);
                }
            }
            if let Some(n) = args.get("min_people").and_then(|v| v.as_u64()) {
                v["min_people"] = json!(n as u32);
            }
            if let Some(b) = args.get("with_speech").and_then(|v| v.as_bool()) {
                v["with_speech"] = json!(b);
            }
            if let Some(x) = args.get("min_secs").and_then(|v| v.as_f64()) {
                v["min_secs"] = json!(x);
            }
            if let Some(n) = args.get("max_hits").and_then(|v| v.as_u64()) {
                v["max_hits"] = json!(n as usize);
            }
            v
        }
        "klipp_contact_sheet" => {
            let mut v = json!({ "method": "contact_sheet", "media_id": arg_str(args, "media_id")? });
            for key in ["from_secs", "to_secs"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_f64()) {
                    v[key] = json!(x);
                }
            }
            for key in ["frames", "cols", "tile_width"] {
                if let Some(n) = args.get(key).and_then(|v| v.as_u64()) {
                    v[key] = json!(n as u32);
                }
            }
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            v
        }
        "klipp_audio_excerpt" => {
            let mut v = json!({
                "method": "audio_excerpt",
                "media_id": arg_str(args, "media_id")?,
                "from_secs": args.get("from_secs").and_then(|v| v.as_f64()).ok_or("missing required argument: from_secs")?,
                "to_secs": args.get("to_secs").and_then(|v| v.as_f64()).ok_or("missing required argument: to_secs")?,
            });
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            v
        }
        "klipp_plan" => json!({ "method": "plan", "ops": args.get("ops").cloned().ok_or("missing required argument: ops")? }),
        "klipp_apply" => {
            let mut v = json!({ "method": "apply", "ops": args.get("ops").cloned().ok_or("missing required argument: ops")? });
            if let Some(l) = args.get("label").and_then(|v| v.as_str()) {
                v["label"] = json!(l);
            }
            v
        }
        "klipp_undo" => json!({ "method": "undo" }),
        "klipp_redo" => json!({ "method": "redo" }),
        "klipp_batch" => {
            let calls = args.get("calls").and_then(|v| v.as_array()).ok_or("missing required argument: calls")?;
            let mut commands = Vec::with_capacity(calls.len());
            for (i, call) in calls.iter().enumerate() {
                let tool = call.get("tool").and_then(|v| v.as_str()).ok_or_else(|| format!("calls[{i}]: missing tool"))?;
                if tool == "klipp_batch" {
                    return Err(format!("calls[{i}]: a batch cannot contain a batch"));
                }
                let inner = call.get("args").cloned().unwrap_or_else(|| json!({}));
                commands.push(tool_to_command(tool, &inner).map_err(|e| format!("calls[{i}] ({tool}): {e}"))?);
            }
            json!({ "method": "batch", "commands": commands })
        }
        "klipp_export" => {
            let mut v = json!({ "method": "export", "path": arg_str(args, "path")? });
            for key in ["width", "height", "crf"] {
                if let Some(n) = args.get(key).and_then(|v| v.as_u64()) {
                    v[key] = json!(n);
                }
            }
            if let Some(f) = args.get("fps").and_then(|v| v.as_f64()) {
                v["fps"] = json!(f);
            }
            if let Some(l) = args.get("target_lufs").and_then(|v| v.as_f64()) {
                v["target_lufs"] = json!(l);
            }
            v
        }
        "klipp_export_status" => json!({ "method": "export_status" }),
        "klipp_export_subtitles" => json!({ "method": "export_subtitles", "path": arg_str(args, "path")? }),
        "klipp_brief" => json!({ "method": "brief" }),
        "klipp_set_brief" => json!({ "method": "set_brief", "brief": args.get("brief").cloned().ok_or("missing required argument: brief")? }),
        "klipp_check" => json!({ "method": "check" }),
        "klipp_save_project" => {
            let mut v = json!({ "method": "save_project" });
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            v
        }
        "klipp_open_project" => json!({
            "method": "open_project",
            "path": arg_str(args, "path")?,
            "discard_changes": args.get("discard_changes").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_frame_at" => {
            let mut v = json!({
                "method": "frame_at",
                "secs": args.get("secs").and_then(|v| v.as_f64()).ok_or("missing required argument: secs")?,
                "max_width": args.get("max_width").and_then(|v| v.as_u64()).unwrap_or(1280),
            });
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            if let Some(t) = args.get("timeout_ms").and_then(|v| v.as_u64()) {
                v["timeout_ms"] = json!(t);
            }
            v
        }
        other => return Err(format!("unknown tool: {other}")),
    })
}

/// How to work here, the craft, and the recipes: files shipped with
/// the app that a user can read and change, served to the agent as
/// text. Not a prompt hidden in code.
const GUIDE: &str = include_str!("../../../assets/craft/guide.md");
const CRAFT: &str = include_str!("../../../assets/craft/reference.md");
const RECIPES: &str = include_str!("../../../assets/craft/recipes.md");

fn call_tool(name: &str, args: &Value) -> Result<Vec<Value>, String> {
    if name == "klipp_guide" {
        let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("guide");
        let text = match topic {
            "guide" => GUIDE.to_string(),
            "craft" => CRAFT.to_string(),
            "recipes" => RECIPES.to_string(),
            "all" => format!("{GUIDE}\n\n---\n\n{CRAFT}\n\n---\n\n{RECIPES}"),
            other => return Err(format!("unknown topic {other}; use guide, craft, recipes or all")),
        };
        return Ok(vec![json!({ "type": "text", "text": text })]);
    }
    let cmd = tool_to_command(name, args)?;
    let result = control_call(cmd)?;

    if WRITE_TOOLS.contains(&name) {
        if let Some(g) = result.get("generation").and_then(|v| v.as_u64()) {
            // Keep the *latest* outstanding write: after a burst of edits
            // the frame worth capturing is one newer than the last of
            // them — a frame newer than only the first may predate the
            // rest. Cleared by the next screenshot.
            let cur = PENDING_WRITE_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
            if cur == u64::MAX || g > cur {
                PENDING_WRITE_GENERATION.store(g, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    if name == "klipp_screenshot" {
        PENDING_WRITE_GENERATION.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
    }

    // The timeline view is text for reading, not JSON for parsing.
    if name == "klipp_timeline" || name == "klipp_media_analysis" || name == "klipp_plan" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            return Ok(vec![json!({ "type": "text", "text": text })]);
        }
    }
    // Pictures and sound come back as a path; inline them too when
    // small enough to be worth a context window.
    let inline = match name {
        "klipp_screenshot" | "klipp_frame_at" => Some(("image", "image/png")),
        "klipp_contact_sheet" => Some(("image", "image/jpeg")),
        "klipp_audio_excerpt" => Some(("audio", "audio/wav")),
        _ => None,
    };
    if let Some((kind, mime)) = inline {
        if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
            let mut content = text_content(&result);
            if let Ok(bytes) = std::fs::read(path) {
                if bytes.len() <= INLINE_IMAGE_MAX_BYTES {
                    content.push(json!({
                        "type": kind,
                        "data": base64_encode(&bytes),
                        "mimeType": mime,
                    }));
                }
            }
            return Ok(content);
        }
    }
    Ok(text_content(&result))
}

/// Standard base64. Hand-rolled to keep this crate at two dependencies;
/// it runs once per screenshot, so its speed is irrelevant.
fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool("klipp_guide",
             "Read this first. How to work in Klipp as an agent (the loop: read, learn the \
              footage, plan, apply, look, deliver; units, ids, anchors, ops, manners), the \
              craft reference (what a professional does without being asked), and the \
              recipes (worked plans for an interview with B-roll, a social cut-down, a \
              montage). Files the user can read and change.",
             json!({ "topic": { "type": "string", "description": "guide (default), craft, recipes, or all" } }),
             vec![]),
        tool("klipp_status",
             "Which build is running (commit, dirty, when it was built, and whether \
              a newer build is on disk so a restart is due), transport state, \
              timeline summary, viewer resolution and the latest performance sample \
              from the running Klipp instance. Start here.",
             json!({}), vec![]),
        tool("klipp_play", "Start playback.", json!({}), vec![]),
        tool("klipp_pause", "Pause playback.", json!({}), vec![]),
        tool("klipp_stop", "Stop playback and return to the start.", json!({}), vec![]),
        tool("klipp_seek",
             "Move the playhead to an absolute position, in seconds from the start \
              of the timeline.",
             json!({ "secs": { "type": "number", "description": "Seconds from timeline start" } }),
             vec!["secs"]),
        tool("klipp_list_clips",
             "Every track and clip: clip ids, timings, speed, blend mode, the media \
              behind each clip, and whether its grade is still an identity. Clip ids \
              from here are what the grade tools take.",
             json!({}), vec![]),
        tool("klipp_list_media",
             "The media pool: media ids, paths, duration, fps, resolution, codec.",
             json!({}), vec![]),
        tool("klipp_import_media",
             "Import a file into the media pool. Asynchronous — the app probes the \
              file on a worker thread, so call klipp_list_media afterwards to see it.",
             json!({ "path": { "type": "string", "description": "Absolute path to a media file" } }),
             vec!["path"]),
        tool("klipp_append_clip",
             "Append a media pool item to the timeline as a new clip. A clip at a different \
              frame rate from the project is added without changing the project's rate.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media" },
                 "track": { "type": "integer", "description": "Track index; omit for the default track" },
             }),
             vec!["media_id"]),
        tool("klipp_set_append_audio",
             "Source patching: whether appending a clip also brings its audio. On suits camera \
              footage; off is for cutting to music, where every clip would otherwise lay down \
              room tone. Shows as V+A / V only in the timeline toolbar, and resets to on when \
              the app restarts.",
             json!({ "enabled": { "type": "boolean", "description": "true brings audio, false is video only" } }),
             vec!["enabled"]),
        tool("klipp_attach_audio",
             "Give a clip the audio of its own media file, linked, spanning exactly what the \
              clip spans. Repairs a clip that was added before appending brought the audio \
              along; refused if the clip already has its audio.",
             json!({ "clip_id": { "type": "string", "description": "From klipp_list_clips" } }),
             vec!["clip_id"]),
        tool("klipp_remove_clip", "Remove a clip from the timeline.",
             json!({ "clip_id": { "type": "string", "description": "From klipp_list_clips" } }),
             vec!["clip_id"]),
        tool("klipp_select_clip",
             "Select a clip (or pass no id to clear the selection). The inspector and \
              colour page follow the selection.",
             json!({ "clip_id": { "type": "string", "description": "Omit to deselect" } }),
             vec![]),
        tool("klipp_get_grade",
             "The clip's colour node graph as JSON, including every node id and its \
              parameters. Read this before changing a grade — node ids come from here.",
             json!({ "clip_id": { "type": "string" } }), vec!["clip_id"]),
        tool("klipp_add_node",
             "Add a colour node to a clip's grade graph. Kind is a NodeKind name, \
              e.g. ColorBalance, Corrector, Curves, Qualifier, Mask, MaskCutout, \
              LayerMixer. A new node is unconnected — wire it with klipp_connect_nodes.",
             json!({
                 "clip_id": { "type": "string" },
                 "kind": { "type": "string", "description": "NodeKind name, e.g. ColorBalance" },
                 "x": { "type": "number", "description": "Canvas x position" },
                 "y": { "type": "number", "description": "Canvas y position" },
             }),
             vec!["clip_id", "kind"]),
        tool("klipp_remove_node", "Remove a node from a clip's grade graph.",
             json!({ "clip_id": { "type": "string" }, "node_id": { "type": "string" } }),
             vec!["clip_id", "node_id"]),
        tool("klipp_connect_nodes",
             "Wire one grade node's output into another's input port.",
             json!({
                 "clip_id": { "type": "string" },
                 "from": { "type": "string", "description": "Source node id" },
                 "from_port": { "type": "integer" },
                 "to": { "type": "string", "description": "Destination node id" },
                 "to_port": { "type": "integer", "description": "0 primary, 1 secondary (e.g. matte)" },
             }),
             vec!["clip_id", "from", "to"]),
        tool("klipp_set_node_param",
             "Set one numeric parameter on a grade node, e.g. temperature, tint, hue, \
              global_mix, saturation, contrast. Call klipp_get_grade first to see what \
              a node currently carries: node ids belong to one clip, so the id of \
              another clip's Corrector is refused rather than applied.",
             json!({
                 "clip_id": { "type": "string" },
                 "node_id": { "type": "string", "description": "From klipp_get_grade for THIS clip" },
                 "param": { "type": "string", "description": "Parameter name, e.g. temperature" },
                 "value": { "type": "number" },
             }),
             vec!["clip_id", "node_id", "param", "value"]),
        tool("klipp_toggle_node", "Enable or disable a grade node (a bypass toggle).",
             json!({ "clip_id": { "type": "string" }, "node_id": { "type": "string" } }),
             vec!["clip_id", "node_id"]),
        tool("klipp_set_log_format",
             "Set the camera log (or HDR) encoding a clip was recorded in, for footage \
              whose file metadata does not say (most camera log files do not). Klipp \
              then decodes it to scene-linear for grading and shows it converted to \
              normal contrast. Applies to the clip's media, so every clip of the same \
              file follows. klipp_list_media shows each file's detected and effective \
              format.",
             json!({
                 "clip_id": { "type": "string" },
                 "format": {
                     "type": "string",
                     "enum": klipp_control::LOG_FORMAT_NAMES,
                     "description": "Srgb for ordinary video; SLog3, VLog, LogC3, CanonLog3, … for camera log"
                 },
             }),
             vec!["clip_id", "format"]),
        tool("klipp_screenshot",
             "Capture what the viewer is currently showing, as a PNG. Returns the file \
              path, and inlines the image when it is small enough. This is how to \
              actually see the result of a grade rather than infer it.",
             json!({
                 "path": { "type": "string", "description": "Where to write the PNG; omit for a temp file" },
                 "max_width": { "type": "integer", "description": "Downscale to this width (default 1280)" },
                 "wait_for_new_frame": {
                     "type": "boolean",
                     "description": "Wait for a frame rendered after the last change. Defaults to \
                                     true when a change was made since the last screenshot, so \
                                     'change then look' captures the result, not the old frame."
                 },
                 "timeout_ms": { "type": "integer", "description": "How long to wait for that frame (default 3000)" },
             }),
             vec![]),
        tool("klipp_perf",
             "Frame timings: decode, grade, composite, total, drops, plus GPU->CPU \
              readback counters. Use it to tell whether a change actually made \
              playback faster.",
             json!({}), vec![]),
        tool("klipp_log_tail",
             "Tail of Klipp's debug log. The engine logs its decisions here, including \
              why the direct-present fast path did or did not engage.",
             json!({ "lines": { "type": "integer", "description": "How many lines (default 100)" } }),
             vec![]),
        tool("klipp_timeline",
             "The timeline as text: tracks top first, every clip with its range in \
              seconds and frames, what it is (media file, title text, generator), its \
              grade and effects in a word, its id, and the markers. Read this before \
              planning an edit; klipp_list_clips is the same as JSON with more fields.",
             json!({ "max_chars": { "type": "integer", "description": "Cut on a line boundary at this many characters (default 12000)" } }),
             vec![]),
        tool("klipp_analyze",
             "Find what is in the footage, in the background: shot boundaries (where the \
              picture cuts), EBU R128 loudness and true peak, the spans of silence, and \
              the transcript with word timestamps and the language when the speech model \
              is downloaded. One media item by id, or every file in the pool when \
              media_id is omitted. Returns at once; poll klipp_analysis_status, then read \
              klipp_media_analysis. Items already analysed cost nothing. Runs on import \
              too, without downloading anything: pass download_models to fetch the \
              148 MB speech model once, with the user's knowledge.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media; omit for every item" },
                 "download_models": { "type": "boolean", "description": "Fetch missing models first (the user should know a download happens)" },
             }),
             vec![]),
        tool("klipp_search_transcripts",
             "Find where something is said: every transcript segment containing the query, \
              case-insensitively, with its media item and its start and end in the file.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" } }),
             vec!["query"]),
        tool("klipp_analysis_status", "Where every requested analysis stands: queued, running, done, failed.",
             json!({}), vec![]),
        tool("klipp_media_analysis",
             "What is known about a media item, as text: its shots with their ranges, \
              its loudness, range and true peak, and its silences. The timeline view \
              shows the same facts per clip in brief. Times are seconds from the file's \
              start; a clip's source_in maps them to the timeline.",
             json!({ "media_id": { "type": "string" }, "max_chars": { "type": "integer", "description": "Cut at this many characters (default 8000)" } }),
             vec!["media_id"]),
        tool("klipp_find_shots",
             "Find shots across every analysed media item: by framing (close-up, medium \
              close-up, medium, wide, very wide, no people), at least so many people, an \
              object the detector named (cat, car, cup…), motion (static, gentle, \
              moving, busy), whether anything is said, a minimum length. Every condition \
              given must hold. Each hit has the media id and the shot's start and end in \
              the file, ready for an edit.",
             json!({
                 "framing": { "type": "string" }, "min_people": { "type": "integer" },
                 "object": { "type": "string" }, "motion": { "type": "string" },
                 "with_speech": { "type": "boolean" }, "min_secs": { "type": "number" },
                 "max_hits": { "type": "integer" },
             }),
             vec![]),
        tool("klipp_contact_sheet",
             "Look at a media file: a grid of frames evenly spaced over a range (the whole \
              file by default), each with its time in the file burnt in, as one JPEG, \
              inlined when small enough. Use it to see what a clip is before planning, \
              and to check a range you cut to. Twelve frames, four to a row, 240 px \
              tiles by default; the reply lists each tile's time.",
             json!({
                 "media_id": { "type": "string" },
                 "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                 "frames": { "type": "integer" }, "cols": { "type": "integer" },
                 "tile_width": { "type": "integer" },
                 "path": { "type": "string", "description": "Absolute JPEG path; temp dir when omitted" },
             }),
             vec!["media_id"]),
        tool("klipp_audio_excerpt",
             "Hear a media file: the sound between two times (at most sixty seconds) as \
              16 kHz mono WAV, inlined when small enough (about thirty seconds). For \
              judging a take, a noise, a music bed.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                     "path": { "type": "string" } }),
             vec!["media_id", "from_secs", "to_secs"]),
        tool("klipp_plan",
             concat!("Dry run: what a list of editorial operations would do, step by step, with each \
              touched clip's range afterwards. Nothing changes. Use it before klipp_apply. ",
              "Editorial operations, resolved by Klipp into frame-exact edits. Give a list of ops; \
              each is an object with \"op\" and its fields:\n\
              place {media_id, track, at, source_in?, source_out?, insert?, with_audio?} — a \
              media item onto a track (an index from klipp_timeline, or \"video\"/\"audio\"/\"text\" \
              for the first of that kind) at an anchor; source_in/out in seconds of the file, \
              the whole file by default; its sound comes along linked on the first audio track \
              (made if none) unless with_audio is false (B-roll over a voice); an empty sequence \
              takes the first picture's size and rate; with insert, every unlocked track is \
              rippled later to make room and a clip straddling the anchor is split.\n\
              add_track {kind, name?} — a video or text track on top (it becomes track 0, every \
              other index moves down by one), an audio or subtitle track at the bottom.\n\
              captions {track?, from?, to?, max_chars?, max_lines?} — captions for what is heard \
              on the timeline, from the transcripts (run klipp_analyze first): each word lands \
              where its clip puts it, cues close at sentences, pauses and cuts, two lines of 42 \
              by default; on the first subtitle track, made at the bottom if there is none. Do \
              it after the speech is placed and trimmed.\n\
              audio {clip_id, volume_db?, gain_db?, pan?, fade_in?, fade_out?} — a clip's sound: \
              volume and pre-fader gain in dB, pan -1..1, fades in seconds; what is left out \
              stays. For an audio clip linked to a video clip the settings go on the video \
              clip, where playback and the render read them.\n\
              duck {clip_id, by_db?, fade_secs?} — music under speech: the clip's volume comes \
              down (9 dB by default) wherever another clip is heard over it, moving over \
              fade_secs (0.5) each way, as volume keyframes. Set the music's volume first, \
              place the speech first.\n\
              lower_third {name, role?, at, secs?, track?, side?, x?, y?} — a name with a role \
              under it, bottom left in title-safe, four seconds by default, fading in and out, on \
              a text track with room (made on top if none: every other index then moves down by \
              one). Refused across a shot change. Names come from the brief, never a guess. Where \
              the speaker is not on the left or the picture has its own name burnt in there, \
              side \"right\" sets it against the right edge, x is that edge and y the name's line, \
              as fractions of the frame inside title-safe.\n\
              set_text {clip_id, text} — the words of a title or a caption, its look, place and \
              timing kept: for a cue the transcript misheard or a name it cannot spell.\n\
              trim {clip_id, edge: \"in\"|\"out\", to, ripple?} — one edge to an anchor, the \
              other stays, the source moves with the edge; with ripple the later clips on the \
              track close up by the change (a head trim then keeps the clip's start).\n\
              split {clip_id, at} — into two, the linked sound with it.\n\
              remove {clip_id, ripple?} — off the track; with ripple the later clips close the gap.\n\
              move {clip_id, to} — the clip's start to an anchor, length unchanged, a linked \
              partner with it.\n\
              j_cut {incoming_clip, frames} — the incoming clip's sound starts so many frames \
              before its picture; l_cut {outgoing_clip, frames} — the outgoing clip's sound \
              stays so many frames past its picture. Both need linked audio on one track.\n\
              Anchors: {\"secs\": s}, {\"frame\": n}, {\"clip_start\": clip}, {\"clip_end\": clip}, \
              {\"track_end\": track} (where the last clip on a track ends: where an assembly's \
              next select goes), {\"marker\": label}, {\"offset\": {\"from\": anchor, \"secs\": s}}. \
              A clip in an anchor, a clip_id (and \
              incoming_clip, outgoing_clip) is an id string, or {\"track\": n, \"secs\": s} for the \
              clip covering that time on that track as the plan stands at that step — how a \
              later step names a half a split made or a clip a place put down.\n\
              Ops run in order, each seeing the ones before it (a split's halves are new ids, \
              reported in the reply). Times snap to frames. Overlaps, locked tracks and edges \
              past the media are refused with why and what would help, and nothing is applied."),
             json!({ "ops": { "type": "array", "items": { "type": "object" } } }),
             vec!["ops"]),
        tool("klipp_apply",
             concat!("Apply a list of editorial operations as ONE undo step, after resolving them the \
              way klipp_plan does; refused whole if any op cannot be done or if the timeline \
              changed since the plan was resolved. Replies with the steps and the revision. ",
              "Editorial operations, resolved by Klipp into frame-exact edits. Give a list of ops; \
              each is an object with \"op\" and its fields:\n\
              place {media_id, track, at, source_in?, source_out?, insert?, with_audio?} — a \
              media item onto a track (an index from klipp_timeline, or \"video\"/\"audio\"/\"text\" \
              for the first of that kind) at an anchor; source_in/out in seconds of the file, \
              the whole file by default; its sound comes along linked on the first audio track \
              (made if none) unless with_audio is false (B-roll over a voice); an empty sequence \
              takes the first picture's size and rate; with insert, every unlocked track is \
              rippled later to make room and a clip straddling the anchor is split.\n\
              add_track {kind, name?} — a video or text track on top (it becomes track 0, every \
              other index moves down by one), an audio or subtitle track at the bottom.\n\
              captions {track?, from?, to?, max_chars?, max_lines?} — captions for what is heard \
              on the timeline, from the transcripts (run klipp_analyze first): each word lands \
              where its clip puts it, cues close at sentences, pauses and cuts, two lines of 42 \
              by default; on the first subtitle track, made at the bottom if there is none. Do \
              it after the speech is placed and trimmed.\n\
              audio {clip_id, volume_db?, gain_db?, pan?, fade_in?, fade_out?} — a clip's sound: \
              volume and pre-fader gain in dB, pan -1..1, fades in seconds; what is left out \
              stays. For an audio clip linked to a video clip the settings go on the video \
              clip, where playback and the render read them.\n\
              duck {clip_id, by_db?, fade_secs?} — music under speech: the clip's volume comes \
              down (9 dB by default) wherever another clip is heard over it, moving over \
              fade_secs (0.5) each way, as volume keyframes. Set the music's volume first, \
              place the speech first.\n\
              lower_third {name, role?, at, secs?, track?, side?, x?, y?} — a name with a role \
              under it, bottom left in title-safe, four seconds by default, fading in and out, on \
              a text track with room (made on top if none: every other index then moves down by \
              one). Refused across a shot change. Names come from the brief, never a guess. Where \
              the speaker is not on the left or the picture has its own name burnt in there, \
              side \"right\" sets it against the right edge, x is that edge and y the name's line, \
              as fractions of the frame inside title-safe.\n\
              set_text {clip_id, text} — the words of a title or a caption, its look, place and \
              timing kept: for a cue the transcript misheard or a name it cannot spell.\n\
              trim {clip_id, edge: \"in\"|\"out\", to, ripple?} — one edge to an anchor, the \
              other stays, the source moves with the edge; with ripple the later clips on the \
              track close up by the change (a head trim then keeps the clip's start).\n\
              split {clip_id, at} — into two, the linked sound with it.\n\
              remove {clip_id, ripple?} — off the track; with ripple the later clips close the gap.\n\
              move {clip_id, to} — the clip's start to an anchor, length unchanged, a linked \
              partner with it.\n\
              j_cut {incoming_clip, frames} — the incoming clip's sound starts so many frames \
              before its picture; l_cut {outgoing_clip, frames} — the outgoing clip's sound \
              stays so many frames past its picture. Both need linked audio on one track.\n\
              Anchors: {\"secs\": s}, {\"frame\": n}, {\"clip_start\": clip}, {\"clip_end\": clip}, \
              {\"track_end\": track} (where the last clip on a track ends: where an assembly's \
              next select goes), {\"marker\": label}, {\"offset\": {\"from\": anchor, \"secs\": s}}. \
              A clip in an anchor, a clip_id (and \
              incoming_clip, outgoing_clip) is an id string, or {\"track\": n, \"secs\": s} for the \
              clip covering that time on that track as the plan stands at that step — how a \
              later step names a half a split made or a clip a place put down.\n\
              Ops run in order, each seeing the ones before it (a split's halves are new ids, \
              reported in the reply). Times snap to frames. Overlaps, locked tracks and edges \
              past the media are refused with why and what would help, and nothing is applied."),
             json!({ "ops": { "type": "array", "items": { "type": "object" } },
                     "label": { "type": "string", "description": "A name for the history entry" } }),
             vec!["ops"]),
        tool("klipp_undo", "One step back in the active sequence's edit history. Every write \
              made through these tools is one step; a klipp_batch is one step as a whole.",
             json!({}), vec![]),
        tool("klipp_redo", "One step forward in the edit history.", json!({}), vec![]),
        tool("klipp_batch",
             "Several write tools applied together in one frame and recorded as ONE undo \
              step: use it for anything that is one action in your plan (an append and \
              its grade, a set of parameter changes). Every call is checked before any \
              is applied; a bad one refuses the whole batch and nothing changes. Reads \
              (status, lists, screenshots) are refused inside a batch. Replies with the \
              document revision after the batch.",
             json!({ "calls": { "type": "array", "description": "In order. Each: {\"tool\": name, \"args\": {...}}",
                 "items": { "type": "object", "properties": {
                     "tool": { "type": "string" }, "args": { "type": "object" } }, "required": ["tool"] } } }),
             vec!["calls"]),
        tool("klipp_export",
             "Render the active sequence to a file with Klipp's default export settings; \
              the container comes from the extension (mp4, mov, mkv, webm). Refused when \
              a clip's media is offline or the path is one of the project's media files. \
              Asynchronous: poll klipp_export_status until the job is complete, then the \
              file is done. The path must be absolute.",
             json!({
                 "path": { "type": "string", "description": "Absolute output path" },
                 "width": { "type": "integer" }, "height": { "type": "integer" },
                 "fps": { "type": "number" },
                 "crf": { "type": "integer", "description": "Quality, lower is better (default 23)" },
                 "target_lufs": { "type": "number", "description": "Loudness normalisation target, e.g. -14 for the web, -23 for broadcast" },
             }),
             vec!["path"]),
        tool("klipp_export_status",
             "Every render job Klipp knows: status (queued, rendering, complete, failed, \
              cancelled), progress 0 to 1, frames, stage, error, loudness result.",
             json!({}), vec![]),
        tool("klipp_export_subtitles",
             "Write the sequence's subtitle and text clips as an SRT file at an absolute path.",
             json!({ "path": { "type": "string" } }), vec!["path"]),
        tool("klipp_brief",
             "The brief: what the piece is for. Its length, platform, picture size, loudness, \
              whether it is captioned and has music, and the cast with names and roles as \
              they should appear on screen. Read it before you plan; if there is none, ask \
              the user for what it should say and set it.",
             json!({}), vec![]),
        tool("klipp_set_brief",
             "Replace the brief, as one undo step. A brief is an object with any of: title, \
              about (what the piece is about), tone, platform, target_secs, width and height \
              (the delivery's picture), loudness_lufs (about -14 for the web, -23 for \
              broadcast), captions (bool), music (bool), cast (a list of {name, role, \
              described}), notes. What is left out asks for nothing. Names in lower thirds \
              come from the cast, klipp_check holds the piece to the brief, and an export \
              that names no size or loudness takes the brief's.",
             json!({ "brief": { "type": "object" } }), vec!["brief"]),
        tool("klipp_check",
             "The check before delivery, as an editor would watch it once: black and flash \
              frames, jump cuts with nothing over the join, titles across a cut or too short \
              to read, captions under a lower third, sound cut inside a word, music left at \
              full level under speech, picture and sound ending apart, offline media, and \
              the brief: length, size, captions, cast, music. Every finding says where, \
              what, and what would put it right. Changes nothing. Run it before klipp_export, \
              fix the errors, decide about the warnings.",
             json!({}), vec![]),
        tool("klipp_save_project",
             "Save the project: to an absolute path (a folder ending in .klipp, made if \
              needed), or where it was last saved when no path is given. Save before a long \
              piece of work and when you are done, so the user can carry on by hand. \
              klipp_status says where the project is saved and whether it has unsaved changes.",
             json!({ "path": { "type": "string" } }), vec![]),
        tool("klipp_open_project",
             "Open the project at an absolute path, replacing the one that is open. Refused \
              while the open project has unsaved changes: save it first, or pass \
              discard_changes true to lose them on purpose.",
             json!({ "path": { "type": "string" }, "discard_changes": { "type": "boolean" } }), vec!["path"]),
        tool("klipp_frame_at",
             "The frame at a time on the timeline, as a PNG, without leaving the playhead \
              where it was: seeks there, waits for the frame, captures, seeks back. Use \
              it to look at what you did at a specific point. Seconds from timeline start.",
             json!({
                 "secs": { "type": "number" },
                 "max_width": { "type": "integer", "description": "Downscale to at most this wide (default 1280)" },
                 "path": { "type": "string", "description": "Absolute PNG path; temp dir when omitted" },
                 "timeout_ms": { "type": "integer" },
             }),
             vec!["secs"]),
    ]
}
