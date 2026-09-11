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
    "klipp_append_clip", "klipp_remove_clip", "klipp_select_clip",
    "klipp_add_node", "klipp_remove_node", "klipp_connect_nodes",
    "klipp_set_node_param", "klipp_toggle_node",
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

fn call_tool(name: &str, args: &Value) -> Result<Vec<Value>, String> {
    let cmd = match name {
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
        other => return Err(format!("unknown tool: {other}")),
    };

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

    // Screenshots come back as a path; inline the image too when it is
    // small enough to be worth a context window.
    if name == "klipp_screenshot" {
        if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
            let mut content = text_content(&result);
            if let Ok(bytes) = std::fs::read(path) {
                if bytes.len() <= INLINE_IMAGE_MAX_BYTES {
                    content.push(json!({
                        "type": "image",
                        "data": base64_encode(&bytes),
                        "mimeType": "image/png",
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
        tool("klipp_status",
             "Transport state, timeline summary, viewer resolution and the latest \
              performance sample from the running Klipp instance. Start here.",
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
             "Append a media pool item to the timeline as a new clip.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media" },
                 "track": { "type": "integer", "description": "Track index; omit for the default track" },
             }),
             vec!["media_id"]),
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
              a node currently carries.",
             json!({
                 "clip_id": { "type": "string" },
                 "node_id": { "type": "string" },
                 "param": { "type": "string", "description": "Parameter name, e.g. temperature" },
                 "value": { "type": "number" },
             }),
             vec!["clip_id", "node_id", "param", "value"]),
        tool("klipp_toggle_node", "Enable or disable a grade node (a bypass toggle).",
             json!({ "clip_id": { "type": "string" }, "node_id": { "type": "string" } }),
             vec!["clip_id", "node_id"]),
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
    ]
}
