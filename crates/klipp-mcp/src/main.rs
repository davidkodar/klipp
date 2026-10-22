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

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc, Mutex};

use serde_json::{json, Value};

mod diagnostics;
mod edit_schema;
mod graph_schema;
mod socket_connect;

const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const STRUCTURED_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_CONCURRENT_READS: usize = 8;
const MAX_QUEUED_WRITES: usize = 8;
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
/// The project generation the last reply carried; `u64::MAX` before any.
static LAST_PROJECT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
static PENDING_WRITE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// Tools that change what the viewer should show. Any of them arms the
/// next screenshot to wait for a fresh frame.
const WRITE_TOOLS: &[&str] = &[
    "klipp_graph", "klipp_import_timeline",
    "klipp_play", "klipp_pause", "klipp_stop", "klipp_seek",
    "klipp_append_clip", "klipp_remove_clip", "klipp_attach_audio", "klipp_normalize", "klipp_set_append_audio", "klipp_select_clip",
    "klipp_add_node", "klipp_remove_node", "klipp_connect_nodes",
    "klipp_set_node_param", "klipp_toggle_node", "klipp_set_log_format",
    "klipp_generator", "klipp_undo", "klipp_redo", "klipp_batch", "klipp_export", "klipp_export_subtitles", "klipp_import_subtitles", "klipp_translate_captions", "klipp_apply",
    "klipp_open_project", "klipp_close_project", "klipp_switch_sequence", "klipp_track_subject",
];

/// Calls with side effects may be replayed after a lost socket reply. The
/// control server owns deduplication, including tools that queue background
/// analysis or export work rather than changing the viewer immediately.
const RETRYABLE_TOOLS: &[&str] = &[
    "klipp_analyze", "klipp_analysis_queue", "klipp_import_media", "klipp_grade_to_target",
    "klipp_adopt_look", "klipp_match_scenes", "klipp_grade_chain", "klipp_review",
    "klipp_narrate", "klipp_record", "klipp_auto_sfx", "klipp_auto_redact",
    "klipp_add_music", "klipp_match_grade", "klipp_align_audio", "klipp_normalize", "klipp_look",
    "klipp_orient", "klipp_set_brief", "klipp_set_edit_plan", "klipp_approve_edit_plan",
    "klipp_build_edit_plan", "klipp_rename_sequence", "klipp_create_multicam",
    "klipp_switch_angle", "klipp_set_multicam_sound", "klipp_add_marker",
    "klipp_update_marker", "klipp_remove_marker", "klipp_new_project", "klipp_new_sequence",
    "klipp_save_project", "klipp_relink_media", "klipp_tag_media",
    "klipp_cancel_job",
];

fn main() {
    diagnostics::build();
    // `klipp-mcp --check`: is Klipp there for an assistant? The same
    // read-only status an assistant asks for first, from the same binary
    // it runs, said in words; exit 0 when it answered (#625). What
    // Preferences → AI runs to check a connection, and a person can run
    // it in a terminal.
    if std::env::args().nth(1).as_deref() == Some("--check") {
        match control_call(json!({ "method": "status" })) {
            Ok(mut v) => {
                diagnostics::attach(&mut v, None);
                println!("{}", v.get("text").and_then(|t| t.as_str()).unwrap_or("Klipp answered."));
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    if std::env::args().nth(1).as_deref() == Some("follow") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(follow_main(&rest));
    }
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("klipp-mcp {}", diagnostics::label());
        return;
    }
    let structured = Arc::new(AtomicBool::new(false));
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Value>(MAX_QUEUED_WRITES);
    let write_stdout = Arc::clone(&stdout);
    let write_structured = Arc::clone(&structured);
    // The channel preserves input order for writes, including edits that
    // refer to clips created by an earlier request. Reads can run while a
    // slow write waits for the UI or a background job.
    let write_worker = std::thread::spawn(move || {
        for req in write_rx {
            send_response(&write_stdout, &response_for(&req, &write_stdout, &write_structured));
        }
    });
    let active_reads = Arc::new(AtomicUsize::new(0));
    let mut read_workers = Vec::new();
    let stdin = std::io::stdin();
    for line in BufReader::new(stdin.lock()).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() { continue; }
        let req: Value = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(error) => {
                send_response(&stdout, &json!({ "jsonrpc":"2.0", "id":null,
                    "error":{"code":-32700,"message":format!("parse error: {error}")} }));
                continue;
            }
        };
        // Notifications carry no id and must get no response at all.
        let Some(id) = req.get("id").cloned() else { continue; };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "tools/call" {
            let name = req["params"]["name"].as_str().unwrap_or("");
            if WRITE_TOOLS.contains(&name) || RETRYABLE_TOOLS.contains(&name) {
                if write_tx.try_send(req).is_err() {
                    send_response(&stdout, &busy_response(id, "write queue is full; retry shortly"));
                }
            } else if active_reads.fetch_update(Ordering::AcqRel, Ordering::Acquire,
                |count| (count < MAX_CONCURRENT_READS).then_some(count + 1)).is_ok() {
                let output = Arc::clone(&stdout);
                let count = Arc::clone(&active_reads);
                let read_structured = Arc::clone(&structured);
                read_workers.push(std::thread::spawn(move || {
                    struct ReadSlot(Arc<AtomicUsize>);
                    impl Drop for ReadSlot { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
                    let _slot = ReadSlot(count);
                    send_response(&output, &response_for(&req, &output, &read_structured));
                }));
            } else {
                send_response(&stdout, &busy_response(id, "eight reads are already in progress; retry shortly"));
            }
            read_workers.retain(|worker| !worker.is_finished());
        } else {
            send_response(&stdout, &response_for(&req, &stdout, &structured));
        }
    }
    drop(write_tx);
    let _ = write_worker.join();
    for worker in read_workers { let _ = worker.join(); }
}

fn send_response(stdout: &Mutex<std::io::Stdout>, response: &Value) {
    let Ok(mut stdout) = stdout.lock() else { return; };
    let _ = writeln!(stdout, "{response}");
    let _ = stdout.flush();
}

fn busy_response(id: Value, message: &str) -> Value {
    json!({ "jsonrpc":"2.0", "id":id, "result":{
        "content":[{"type":"text","text":message}], "isError":true
    }})
}

fn response_for(req: &Value, stdout: &Mutex<std::io::Stdout>, structured: &AtomicBool) -> Value {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    match method {
            "initialize" => {
                let version = negotiated_version(req.get("params").and_then(|p| p.get("protocolVersion")).and_then(Value::as_str));
                structured.store(version == STRUCTURED_PROTOCOL_VERSION, Ordering::Release);
                json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "klipp", "version": diagnostics::label() },
                    "_meta": diagnostics::metadata(),
                    "instructions": INSTRUCTIONS,
                }
            })},
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions_for(structured.load(Ordering::Acquire)), "_meta": diagnostics::metadata() },
            }),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let progress_token = params.get("_meta").and_then(|meta| meta.get("progressToken"));
                let result = if name == "klipp_follow_job" {
                    follow_job(&args, progress_token, stdout).map(|content| (content, None))
                } else { call_tool(name, &args) };
                match result {
                    Ok((content, raw)) => {
                        let mut result = json!({ "content": content, "isError": false });
                        if structured.load(Ordering::Acquire) && has_structured_output(name) {
                            if let Some(raw) = raw { result["structuredContent"] = raw; }
                        }
                        json!({ "jsonrpc": "2.0", "id": id, "result": result })
                    }
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
    }
}

/// Follow a job to its end through bounded control reads (#732), calling
/// `step` with the job and its line in words whenever what a person is
/// told changes: the state, the phase, or the percentage by `every`
/// points. Returns the last read and whether it ended, or why it could
/// not be read. `None` for `max_wait` follows to the end.
fn follow(job_id: &str, max_wait: Option<std::time::Duration>, every: i64, mut step: impl FnMut(&Value, &str)) -> Result<(Value, bool), String> {
    let started = std::time::Instant::now();
    let mut told: Option<(String, String, i64)> = None;
    loop {
        let left = max_wait.map(|w| w.saturating_sub(started.elapsed()));
        let timeout = left.unwrap_or(std::time::Duration::from_secs(10)).as_millis().min(10_000) as u64;
        let result = control_call(json!({ "method": "job_status", "job_id": job_id, "wait": told.is_some(), "timeout_ms": timeout }))?;
        let job = result["jobs"].as_array().and_then(|jobs| jobs.first()).cloned().ok_or("job status had no job")?;
        let state = job["state"].as_str().unwrap_or("unknown").to_owned();
        let phase = job["phase"].as_str().unwrap_or(&state).to_owned();
        let percent = job["progress"].as_f64().map(|p| (p * 100.0).floor() as i64).unwrap_or(-1);
        let terminal = matches!(state.as_str(), "completed" | "failed" | "cancelled" | "unavailable");
        let news = match &told {
            None => true,
            Some((s, p, at)) => *s != state || *p != phase || terminal || percent >= at + every.max(1),
        };
        if news {
            let line = result["text"].as_str().and_then(|t| t.lines().next()).map(str::to_owned).unwrap_or_else(|| job_line(&job));
            step(&job, &line);
            told = Some((state, phase, percent));
        }
        if terminal {
            return Ok((job, true));
        }
        if left.is_some_and(|l| l.is_zero()) {
            return Ok((job, false));
        }
    }
}

/// A job in words when Klipp's reply carries no line of its own.
fn job_line(job: &Value) -> String {
    let state = job["state"].as_str().unwrap_or("unknown");
    let mut line = format!("{} {state}", job["kind"].as_str().unwrap_or("job"));
    if let Some(p) = job["progress"].as_f64().filter(|_| state != "completed") {
        line.push_str(&format!(" {:.0}%", (p * 100.0).floor()));
    }
    let ended = matches!(state, "completed" | "failed" | "cancelled" | "unavailable");
    if let Some(phase) = job["phase"].as_str().filter(|p| *p != state && !ended) {
        line.push_str(&format!(": {phase}"));
    }
    if let Some(artifact) = job["artifact"].as_str() {
        line.push_str(&format!(" — {artifact}"));
    }
    if let Some(error) = job["error"].as_str() {
        line.push_str(&format!("; {error}"));
    }
    line
}

fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// `klipp_follow_job`: wait for the job to end (#732) and say how it
/// ended, with the way there. An MCP client's progress token also gets
/// throttled notifications on the way, for a client that shows them;
/// the model sees only this reply, so the reply carries it all.
fn follow_job(args: &Value, token: Option<&Value>, stdout: &Mutex<std::io::Stdout>) -> Result<Vec<Value>, String> {
    let job_id = arg_str(args, "job_id")?;
    let token = token.filter(|value| value.is_string() || value.is_number());
    let max_wait = std::time::Duration::from_millis(args.get("max_wait_ms").and_then(Value::as_u64).unwrap_or(600_000).clamp(1, 3_600_000));
    let started = std::time::Instant::now();
    let mut history: Vec<String> = Vec::new();
    let mut last_line = String::new();
    let (job, ended) = follow(&job_id, Some(max_wait), 10, |job, line| {
        let percent = job["progress"].as_f64().map(|p| (p * 100.0).floor() as i64);
        let phase = job["phase"].as_str().unwrap_or("");
        history.push(match percent {
            Some(p) => format!("{} {p}% {phase}", clock(started.elapsed())),
            None => format!("{} {phase}", clock(started.elapsed())),
        });
        if let Some(token) = token {
            let mut params = json!({ "progressToken": token, "progress": percent.unwrap_or(0), "message": line });
            if percent.is_some() { params["total"] = json!(100); }
            send_response(stdout, &json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": params }));
        }
        last_line = line.to_owned();
    })?;
    let mut text = last_line;
    if ended {
        text.push_str(&format!("\nEnded after {} of following.", clock(started.elapsed())));
    } else {
        text.push_str(&format!("\nStill {} after {}: call klipp_follow_job again to keep waiting.",
            job["state"].as_str().unwrap_or("running"), clock(started.elapsed())));
    }
    if history.len() > 1 {
        text.push_str(&format!("\nOn the way: {}.", history.join(" · ")));
    }
    if job["state"] == "completed" && job["kind"] == "render" {
        text.push_str("\nThe file is written; the render is done.");
    }
    Ok(vec![json!({ "type": "text", "text": text })])
}

/// `klipp-mcp follow <job_id> [--every N]`: the job's progress as lines
/// on standard output, one whenever its state or phase changes or it
/// moves N percent (10 unless given), the last when it ends; exit 0 when
/// it completed, 1 when it failed or was cancelled, 2 when it could not
/// be read (#732). An agent's harness runs it in the background and
/// hears each line as it comes — the push MCP itself does not give.
fn follow_main(args: &[String]) -> i32 {
    let Some(job_id) = args.first() else {
        eprintln!("usage: klipp-mcp follow <job_id> [--every N]");
        return 2;
    };
    let every = args.iter().position(|a| a == "--every").and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok()).unwrap_or(10);
    let started = std::time::Instant::now();
    let out = follow(job_id, None, every, |_, line| {
        println!("{} {line}", clock(started.elapsed()));
        use std::io::Write;
        let _ = std::io::stdout().flush();
    });
    match out {
        Ok((job, _)) if job["state"] == "completed" => 0,
        Ok(_) => 1,
        Err(e) => {
            println!("{} could not follow {job_id}: {e}", clock(started.elapsed()));
            2
        }
    }
}

fn negotiated_version(client: Option<&str>) -> &'static str {
    if matches!(client, Some("2025-06-18" | "2025-11-25")) {
        STRUCTURED_PROTOCOL_VERSION
    } else {
        LEGACY_PROTOCOL_VERSION
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
    let method = command.get("method").and_then(Value::as_str).unwrap_or("");
    let job_poll = method == "job_status";
    // Some tools deliberately hold their control reply until the work is
    // done. Keep that reply intact; the stdio read worker leaves ping and
    // other tools responsive while it waits.
    // The colour tools read every shot they grade, each clip rendered
    // alone (#719, #720): a whole piece is a minute or more on a busy
    // machine.
    let long_call = matches!(method, "review" | "record" | "analyze" | "narrate"
        | "search_music" | "search_sfx" | "add_music" | "grade_to_target" | "match_scenes" | "timeline_sheet" | "mix_loudness")
        || (method == "check" && command.get("color").and_then(Value::as_bool) == Some(true))
        || (!job_poll && command.get("wait").and_then(Value::as_bool) == Some(true));
    let read_timeout_ms = std::env::var("KLIPP_MCP_SOCKET_TIMEOUT_MS").ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(250, 120_000)).unwrap_or(32_000);
    // Said so it can be passed on as it is: what the person should do
    // first, the detail after (#626).
    let stream = socket_connect::connect(&path, std::time::Duration::from_secs(2)).map_err(|e| {
        format!(
            "Klipp does not seem to be running: start Klipp, then try again. \
             (Technical detail: no answer on {}: {e}. The connection opens once \
             the app has started; KLIPP_CONTROL=0 turns it off.)",
            path.display(),
        )
    })?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("setting Klipp socket write timeout failed: {e}"))?;
    stream.set_read_timeout((!long_call).then(|| std::time::Duration::from_millis(read_timeout_ms)))
        .map_err(|e| format!("setting Klipp socket read timeout failed: {e}"))?;
    let mut writer = stream.try_clone().map_err(|e| format!("socket clone failed: {e}"))?;
    let mut reader = BufReader::new(stream);

    let mut payload = command;
    // Only job polling is short and repeatable. Other wait:true calls keep
    // the caller's timeout, since transcription and recording may take minutes.
    if job_poll && payload.get("wait").and_then(Value::as_bool) == Some(true) {
        let bounded = payload.get("timeout_ms").and_then(Value::as_u64).unwrap_or(10_000)
            .min(10_000).min(read_timeout_ms.saturating_sub(200));
        payload["timeout_ms"] = json!(bounded);
    }
    let retry_key = payload.get("idempotency_key").and_then(Value::as_str).map(str::to_owned);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("id".into(), json!(1));
    }
    let mut text = payload.to_string();
    text.push('\n');
    writer
        .write_all(text.as_bytes())
        .map_err(|e| uncertain_socket_error("sending", &e, retry_key.as_deref()))?;
    writer.flush().ok();

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| uncertain_socket_error("reading", &e, retry_key.as_deref()))?;
    if line.trim().is_empty() {
        return Err(uncertain_socket_error("reading", &std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Klipp closed the connection without replying"), retry_key.as_deref()));
    }
    let reply: Value =
        serde_json::from_str(&line).map_err(|e| format!("bad reply from Klipp: {e}"))?;
    if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(reply.get("result").cloned().unwrap_or(json!({})))
    } else if reply.get("result").is_some_and(|r| r.get("why").is_some()) {
        Err(reply["result"].to_string())
    } else {
        Err(reply
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string())
    }
}

fn uncertain_socket_error(phase: &str, error: &std::io::Error, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("{phase} from Klipp failed: {error}. The write outcome is unknown; retry the same call with idempotency_key {key:?}, then read klipp_operation_status using the returned operation_id. Do not send a new key for the same edit."),
        None => format!("{phase} from Klipp failed: {error}. If this was a write, its outcome is unknown; inspect the timeline before retrying."),
    }
}

/// `list_clips` as JSON with a clip to a line (#507): pretty-printed, a
/// clip's every field took a line of its own, and 70 clips came to 63k
/// characters.
fn clips_text(v: &Value) -> String {
    let mut head = v.clone();
    let tracks = head.as_object_mut().and_then(|o| o.remove("tracks")).unwrap_or(json!([]));
    let mut out = head.to_string();
    out.pop();
    if out.len() > 1 {
        out.push_str(", ");
    }
    out.push_str("\"tracks\": [");
    let tracks = tracks.as_array().cloned().unwrap_or_default();
    for (i, t) in tracks.iter().enumerate() {
        let mut t = t.clone();
        let clips = t.as_object_mut().and_then(|o| o.remove("clips")).unwrap_or(json!([]));
        let mut line = t.to_string();
        line.pop();
        let clips: Vec<String> = clips.as_array().map(|c| c.iter().map(|c| format!("\n    {c}")).collect()).unwrap_or_default();
        out.push_str(&format!("\n  {line}, \"clips\": [{}]}}{}", clips.join(","), if i + 1 < tracks.len() { "," } else { "" }));
    }
    out.push_str("\n]}");
    out
}

fn text_content(v: &Value) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    })]
}

/// A search's `listen`: a count, or a list of result ids (#795).
fn listen_arg(args: &Value) -> Option<Value> {
    match args.get("listen")? {
        Value::Number(n) => n.as_u64().map(|n| json!(n)),
        Value::Array(ids) => Some(json!(ids.iter().filter_map(|i| match i {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }).collect::<Vec<_>>())),
        _ => None,
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

/// Validate selection and save/discard choices without silently coercing them.
fn project_switch_command(method: &str, args: &Value) -> Result<Value, String> {
    let mut out=json!({"method":method});
    for key in ["save","discard_changes"] {
        let value=match args.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            _ => return Err(format!("{key} must be a boolean")),
        };
        out[key]=json!(value);
    }
    if out["save"] == true && out["discard_changes"] == true { return Err("choose save or discard_changes, not both".into()); }
    if method == "open_project" {
        for key in ["path","name"] {
            out[key]=match args.get(key) {
                None | Some(Value::Null) => Value::Null,
                Some(Value::String(s)) => json!(s),
                _ => return Err(format!("{key} must be a string")),
            };
        }
        if out["path"].is_null() == out["name"].is_null() { return Err("give exactly one of path or name".into()); }
    }
    Ok(out)
}

/// The socket command a tool call is, shared by single and batch calls.
fn tool_to_command(name: &str, args: &Value) -> Result<Value, String> {
    if matches!(name, "klipp_plan" | "klipp_apply") {
        edit_schema::validate_ops(args)?;
    }
    let mut command = match name {
        "klipp_status" => json!({ "method": "status" }),
        "klipp_operation_status" => json!({ "method": "operation_status", "operation_id": arg_str(args, "operation_id")? }),
        "klipp_job_status" => json!({ "method": "job_status", "job_id": args.get("job_id"),
            "wait": args.get("wait").and_then(Value::as_bool).unwrap_or(false),
            "timeout_ms": args.get("timeout_ms") }),
        "klipp_follow_job" => json!({ "method": "job_status", "job_id": arg_str(args, "job_id")?,
            "wait": true, "timeout_ms": args.get("max_wait_ms") }),
        "klipp_cancel_job" => json!({ "method":"cancel_job", "job_id":arg_str(args, "job_id")? }),
        "klipp_play" => json!({ "method": "play" }),
        "klipp_pause" => json!({ "method": "pause" }),
        "klipp_stop" => json!({ "method": "stop" }),
        "klipp_seek" => json!({
            "method": "seek",
            "secs": args.get("secs").and_then(|v| v.as_f64())
                .ok_or("missing required argument: secs")?,
        }),
        "klipp_generator" => json!({
            "method": if args.get("apply").and_then(Value::as_bool).unwrap_or(false) { "apply" } else { "plan" },
            "ops": [{ "op": "generator", "data": args.get("data").ok_or("missing required argument: data")?, "at": { "secs": args.get("at_secs").and_then(Value::as_f64).ok_or("missing required argument: at_secs")? }, "secs": args.get("secs").and_then(Value::as_f64).ok_or("missing required argument: secs")?, "track": args.get("track") }],
        }),
        "klipp_list_clips" => json!({
            "method": "list_clips",
            "full": args.get("full").and_then(Value::as_bool).unwrap_or(false),
            "track": args.get("track"),
            "from_secs": args.get("from_secs").and_then(Value::as_f64),
            "to_secs": args.get("to_secs").and_then(Value::as_f64),
            "media_id": args.get("media_id").and_then(Value::as_str),
        }),
        "klipp_list_media" => json!({ "method": "list_media" }),
        "klipp_import_media" => {
            let mut v = json!({ "method": "import_media", "analyze": args.get("analyze").and_then(Value::as_bool).unwrap_or(true) });
            if let Some(p) = args.get("path").and_then(Value::as_str) { v["path"] = json!(p); }
            if let Some(ps) = args.get("paths").filter(|p| !p.is_null()) { v["paths"] = ps.clone(); }
            if let Some(f) = args.get("folder").and_then(Value::as_str) { v["folder"] = json!(f); }
            if v.get("path").is_none() && v.get("paths").is_none() && v.get("folder").is_none() {
                return Err("klipp_import_media needs path, paths or folder".into());
            }
            v
        }
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
        "klipp_normalize" => {
            let ids = args.get("clip_ids").and_then(|v| v.as_array()).ok_or("clip_ids must be a list of clip ids")?;
            let mut v = json!({ "method": "normalize_clips", "clip_ids": ids });
            for key in ["mode", "target", "ceiling_dbtp", "group", "apply"] {
                if let Some(x) = args.get(key) {
                    v[key] = x.clone();
                }
            }
            v
        }
        "klipp_set_append_audio" => json!({
            "method": "set_append_audio",
            "enabled": args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        }),
        "klipp_detect_scene_cuts" => json!({ "method": "detect_scene_cuts", "clip_id": arg_str(args, "clip_id")? }),
        "klipp_track_surface" => {
            let mut v = json!({
                "method": "track_surface",
                "clip_id": arg_str(args, "clip_id")?,
            });
            for key in ["corners", "click", "at_secs", "name", "surface", "direction"] {
                if let Some(x) = args.get(key).filter(|x| !x.is_null()) {
                    v[key] = x.clone();
                }
            }
            v
        }
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
        "klipp_set_node_param" => {
            let remove = args.get("remove").and_then(|v| v.as_bool()).unwrap_or(false);
            let value = args.get("value").and_then(|v| v.as_f64());
            if value.is_none() && !remove {
                return Err("missing required argument: value".into());
            }
            let mut req = json!({
                "method": "set_node_param",
                "clip_id": arg_str(args, "clip_id")?,
                "node_id": arg_str(args, "node_id")?,
                "param": arg_str(args, "param")?,
                "value": value.map(|v| v as f32),
                "remove": remove,
            });
            // A key at a time on the timeline (#891).
            if let Some(at) = args.get("at").and_then(|v| v.as_f64()) {
                req["at"] = json!(at);
            }
            if let Some(i) = args.get("interpolation").and_then(|v| v.as_str()) {
                req["interpolation"] = json!(i);
            }
            req
        }
        "klipp_toggle_node" => json!({
            "method": "toggle_node_enabled",
            "clip_id": arg_str(args, "clip_id")?,
            "node_id": arg_str(args, "node_id")?,
        }),
        "klipp_rename_node" => json!({
            "method": "rename_node",
            "clip_id": arg_str(args, "clip_id")?,
            "node_id": arg_str(args, "node_id")?,
            "name": arg_str(args, "name")?,
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
            if let Some(n) = args.get("track").and_then(|v| v.as_u64()) {
                v["track"] = json!(n as usize);
            }
            for key in ["from_secs", "to_secs"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_f64()) {
                    v[key] = json!(x);
                }
            }
            v
        }
        "klipp_analyze" => {
            let mut v = json!({ "method": "analyze", "wait": args.get("wait").and_then(Value::as_bool).unwrap_or(false), "timeout_ms": args.get("timeout_ms") });
            if let Some(id) = args.get("media_id").and_then(|v| v.as_str()) {
                v["media_id"] = json!(id);
            }
            if let Some(d) = args.get("download_models").and_then(|v| v.as_bool()) {
                v["download_models"] = json!(d);
            }
            v
        }
        "klipp_track_subject" => {
            let mut v = json!({ "method": "track_subject" });
            // A clip, or a piece transition cutting round its subject (#985).
            for key in ["clip_id", "transition_id"] {
                if let Some(id) = args.get(key).and_then(|v| v.as_str()) {
                    v[key] = json!(id);
                }
            }
            if v.get("clip_id").is_none() && v.get("transition_id").is_none() {
                return Err("clip_id or transition_id is required".into());
            }
            if let Some(m) = args.get("model").and_then(|v| v.as_str()) {
                v["model"] = json!(m);
            }
            // Grade (the default) or cut the clip out round it (#73).
            if let Some(i) = args.get("into").and_then(|v| v.as_str()) {
                v["into"] = json!(i);
            }
            if let Some(d) = args.get("download_model").and_then(|v| v.as_bool()) {
                v["download_model"] = json!(d);
            }
            v
        }
        "klipp_screen_text" => {
            let mut v = json!({ "method": "screen_text", "media_id": arg_str(args, "media_id")? });
            if let Some(f) = args.get("from_secs").and_then(|v| v.as_f64()) { v["from_secs"] = json!(f); }
            if let Some(t) = args.get("to_secs").and_then(|v| v.as_f64()) { v["to_secs"] = json!(t); }
            if let Some(a) = args.get("all").and_then(|v| v.as_bool()) { v["all"] = json!(a); }
            v
        }
        "klipp_search_screen_text" => {
            let mut v = json!({ "method": "search_screen_text", "query": arg_str(args, "query")? });
            if let Some(n) = args.get("max_hits").and_then(|v| v.as_u64()) {
                v["max_hits"] = json!(n as usize);
            }
            v
        }
        "klipp_search_visual" => {
            let mut v = json!({ "method": "search_visual", "query": arg_str(args, "query")? });
            if let Some(n) = args.get("max_hits").and_then(|v| v.as_u64()) {
                v["max_hits"] = json!(n as usize);
            }
            if let Some(s) = args.get("min_score").and_then(|v| v.as_f64()) {
                v["min_score"] = json!(s);
            }
            v
        }
        "klipp_auto_redact" => json!({ "method": "auto_redact", "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_skin_balance" => json!({ "method": "skin_balance", "clip_id": arg_str(args, "clip_id")?, "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_grade_to_target" => json!({
            "method": "grade_to_target",
            "clip_id": args.get("clip_id").cloned().unwrap_or(serde_json::Value::Null),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "rounds": args.get("rounds").cloned().unwrap_or(serde_json::Value::Null),
            "force": args.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
            "strength": args.get("strength").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_audio_spectrum" => json!({
            "method": "audio_spectrum",
            "media_id": args.get("media_id").and_then(|v| v.as_str()).ok_or("missing required argument: media_id")?,
            "from_secs": args.get("from_secs").cloned().unwrap_or(serde_json::Value::Null),
            "to_secs": args.get("to_secs").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_adopt_look" => json!({
            "method": "adopt_look",
            "media_id": args.get("media_id").cloned().unwrap_or(serde_json::Value::Null),
            "from_timeline": args.get("from_timeline").and_then(|v| v.as_bool()).unwrap_or(false),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_match_scenes" => json!({
            "method": "match_scenes",
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "rounds": args.get("rounds").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_grade_chain" => json!({
            "method": "grade_chain",
            "clip_id": args.get("clip_id").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_colour_selfcheck" => json!({
            "method": "colour_selfcheck",
            "size": args.get("size").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_measure_region" => json!({
            "method": "measure_region",
            "secs": args.get("secs").and_then(|v| v.as_f64()).ok_or("missing required argument: secs")?,
            "x": args.get("x").and_then(|v| v.as_f64()).ok_or("missing required argument: x")?,
            "y": args.get("y").and_then(|v| v.as_f64()).ok_or("missing required argument: y")?,
            "w": args.get("w").and_then(|v| v.as_f64()).ok_or("missing required argument: w")?,
            "h": args.get("h").and_then(|v| v.as_f64()).ok_or("missing required argument: h")?,
            "timeout_ms": args.get("timeout_ms").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_review" => json!({
            "method": "review",
            "max_width": args.get("max_width").cloned().unwrap_or(serde_json::Value::Null),
            "frames": args.get("frames").cloned().unwrap_or(serde_json::Value::Null),
            "cols": args.get("cols").cloned().unwrap_or(serde_json::Value::Null),
            "path": args.get("path").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_narrate" => json!({
            "method": "narrate",
            "text": arg_str(args, "text")?,
            "at_secs": args.get("at_secs").cloned().unwrap_or(serde_json::Value::Null),
            "voice": args.get("voice").cloned().unwrap_or(serde_json::Value::Null),
            "speed": args.get("speed").cloned().unwrap_or(serde_json::Value::Null),
            "pause_secs": args.get("pause_secs").cloned().unwrap_or(serde_json::Value::Null),
            "fit_secs": args.get("fit_secs").cloned().unwrap_or(serde_json::Value::Null),
            "variation": args.get("variation").cloned().unwrap_or(serde_json::Value::Null),
            "download_models": args.get("download_models").and_then(|v| v.as_bool()).unwrap_or(false),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "path": args.get("path").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_record" => {
            let mut v = json!({ "method": "record" });
            for key in ["secs", "target", "fps", "path", "import", "wait", "action", "recording_id", "codec", "quality"] {
                if let Some(x) = args.get(key).filter(|x| !x.is_null()) { v[key] = x.clone(); }
            }
            v
        }
        "klipp_audition" => {
            let mut v = json!({ "method": "audition" });
            for key in ["action", "media_id", "clip_id", "sequence", "from_secs", "to_secs", "preview_urls"] {
                if let Some(x) = args.get(key).filter(|x| !x.is_null()) { v[key] = x.clone(); }
            }
            v
        }
        "klipp_mix_loudness" => json!({
            "method": "mix_loudness",
            "from_secs": args.get("from_secs").cloned().unwrap_or(serde_json::Value::Null),
            "to_secs": args.get("to_secs").cloned().unwrap_or(serde_json::Value::Null),
            "step_secs": args.get("step_secs").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_auto_sfx" => json!({
            "method": "auto_sfx",
            "whoosh": args.get("whoosh").cloned().unwrap_or(serde_json::Value::Null),
            "click": args.get("click").cloned().unwrap_or(serde_json::Value::Null),
            "riser": args.get("riser").cloned().unwrap_or(serde_json::Value::Null),
            "on": args.get("on").cloned().unwrap_or(serde_json::Value::Null),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_search_transcripts" => {
            let mut v = json!({ "method": "search_transcripts", "query": arg_str(args, "query")? });
            if let Some(n) = args.get("max_hits").and_then(|v| v.as_u64()) {
                v["max_hits"] = json!(n as usize);
            }
            v
        }
        "klipp_analysis_queue" => json!({ "method": "analysis_queue", "media_id": args.get("media_id"), "cancel_pending": args.get("cancel_pending").and_then(Value::as_bool).unwrap_or(false) }),
        "klipp_analysis_status" => json!({ "method": "analysis_status", "wait": args.get("wait").and_then(Value::as_bool).unwrap_or(false), "media_id": args.get("media_id"), "timeout_ms": args.get("timeout_ms") }),
        "klipp_media_analysis" => {
            let mut v = json!({ "method": "media_analysis", "media_id": arg_str(args, "media_id")? });
            if let Some(n) = args.get("max_chars").and_then(|v| v.as_u64()) {
                v["max_chars"] = json!(n as usize);
            }
            v
        }
        "klipp_beat_sheet" => json!({ "method": "beat_sheet", "media_id": arg_str(args, "media_id")? }),
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
            if let Some(b) = args.get("inline").and_then(|v| v.as_bool()) {
                v["inline"] = json!(b);
            }
            v
        }
        "klipp_audio_excerpt" => {
            let mut v = json!({
                "method": "audio_excerpt",
                "from_secs": args.get("from_secs").and_then(|v| v.as_f64()).ok_or("missing required argument: from_secs")?,
                "to_secs": args.get("to_secs").and_then(|v| v.as_f64()).ok_or("missing required argument: to_secs")?,
            });
            if let Some(m) = args.get("media_id").and_then(|v| v.as_str()) {
                v["media_id"] = json!(m);
            }
            if let Some(s) = args.get("sequence").and_then(|v| v.as_bool()) {
                v["sequence"] = json!(s);
            }
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            v
        }
        "klipp_plan" => json!({
            "method": "plan",
            "ops": args.get("ops").cloned().ok_or("missing required argument: ops")?,
            "detail": args.get("detail").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_apply" => {
            let mut v = json!({
                "method": "apply",
                "ops": args.get("ops").cloned().ok_or("missing required argument: ops")?,
                "detail": args.get("detail").and_then(|v| v.as_bool()).unwrap_or(false),
            });
            for key in ["expected_revision", "expected_project_identity"] {
                if let Some(value) = args.get(key) {
                    let number = value.as_u64().ok_or_else(|| format!("{key} must be a non-negative integer"))?;
                    v[key] = json!(number);
                }
            }
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
                if inner.get("idempotency_key").is_some() {
                    return Err(format!("calls[{i}] ({tool}): put idempotency_key on the batch, not an inner call"));
                }
                commands.push(tool_to_command(tool, &inner).map_err(|e| format!("calls[{i}] ({tool}): {e}"))?);
            }
            json!({ "method": "batch", "commands": commands })
        }
        "klipp_export" => {
            let mut v = json!({ "method": "export", "path": arg_str(args, "path")? });
            for key in ["width", "height", "crf", "bit_depth", "bitrate_kbps"] {
                if let Some(n) = args.get(key).and_then(|v| v.as_u64()) {
                    v[key] = json!(n);
                }
            }
            for key in ["preset", "codec", "audio_codec", "encoder_speed"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_str()) {
                    v[key] = json!(x);
                }
            }
            if let Some(f) = args.get("fps").and_then(|v| v.as_f64()) {
                v["fps"] = json!(f);
            }
            if let Some(l) = args.get("target_lufs").and_then(|v| v.as_f64()) {
                v["target_lufs"] = json!(l);
            }
            if let Some(c) = args.get("true_peak_dbtp").and_then(|v| v.as_f64()) {
                v["true_peak_dbtp"] = json!(c);
            }
            if let Some(b) = args.get("captions_608").and_then(|v| v.as_bool()) {
                v["captions_608"] = json!(b);
            }
            if let Some(b) = args.get("hdr10plus").and_then(|v| v.as_bool()) {
                v["hdr10plus"] = json!(b);
            }
            if let Some(b) = args.get("burn_captions").and_then(|v| v.as_bool()) {
                v["burn_captions"] = json!(b);
            }
            v
        }
        "klipp_transcript" => {
            let mut v = json!({ "method": "transcript", "media_id": arg_str(args, "media_id")?, "words": args.get("words").and_then(|v| v.as_bool()).unwrap_or(false) });
            for key in ["from_secs", "to_secs"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_f64()) {
                    v[key] = json!(x);
                }
            }
            v
        }
        "klipp_cut_points" => {
            let mut v = json!({ "method": "cut_points", "media_id": arg_str(args, "media_id")? });
            for key in ["near_secs", "from_secs", "to_secs"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_f64()) {
                    v[key] = json!(x);
                }
            }
            v
        }
        "klipp_prepare_cut" => json!({
            "method": "prepare_cut",
            "from_secs": args.get("from_secs").and_then(Value::as_f64).ok_or("missing required argument: from_secs")?,
            "to_secs": args.get("to_secs").and_then(Value::as_f64).ok_or("missing required argument: to_secs")?,
            "near_secs": args.get("near_secs"),
            "intent": args.get("intent"),
            "max_candidates": args.get("max_candidates"),
        }),
        "klipp_selects" => {
            let mut v = json!({ "method": "selects", "media_id": arg_str(args, "media_id")? });
            if let Some(x) = args.get("min_gap_secs").and_then(|v| v.as_f64()) { v["min_gap_secs"] = json!(x); }
            v
        }
        "klipp_beats" => {
            let mut v = json!({ "method": "beats", "media_id": arg_str(args, "media_id")? });
            for key in ["target_secs", "at_secs"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_f64()) { v[key] = json!(x); }
            }
            if let Some(t) = args.get("track") { v["track"] = json!(t.as_u64().map(|i| i.to_string()).or_else(|| t.as_str().map(str::to_string))); }
            if let Some(b) = args.get("skip_intro").and_then(|v| v.as_bool()) { v["skip_intro"] = json!(b); }
            v
        }
        "klipp_search_music" => {
            let mut v = json!({ "method": "search_music", "query": arg_str(args, "query")? });
            if let Some(x) = args.get("limit").and_then(|v| v.as_u64()) { v["limit"] = json!(x); }
            if let Some(c) = args.get("commercial_only").and_then(|v| v.as_bool()) { v["commercial_only"] = json!(c); }
            if let Some(c) = args.get("instrumental").and_then(|v| v.as_bool()) { v["instrumental"] = json!(c); }
            if let Some(c) = args.get("edits_only").and_then(|v| v.as_bool()) { v["edits_only"] = json!(c); }
            if let Some(l) = args.get("licences").filter(|l| l.is_array()) { v["licences"] = l.clone(); }
            if let Some(x) = listen_arg(args) { v["listen"] = x; }
            v
        }
        "klipp_search_sfx" => {
            let mut v = json!({ "method": "search_sfx", "query": arg_str(args, "query")? });
            if let Some(x) = args.get("limit").and_then(|v| v.as_u64()) { v["limit"] = json!(x); }
            if let Some(x) = args.get("max_secs").and_then(|v| v.as_f64()) { v["max_secs"] = json!(x); }
            if let Some(c) = args.get("commercial_only").and_then(|v| v.as_bool()) { v["commercial_only"] = json!(c); }
            if let Some(x) = listen_arg(args) { v["listen"] = x; }
            v
        }
        "klipp_add_music" => {
            let mut v = json!({ "method": "add_music" });
            for key in ["title", "artist", "track_id", "download_url", "preview_url", "license", "license_url", "page_url", "quality"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_str()) { v[key] = json!(x); }
            }
            if let Some(t) = args.get("tracks").filter(|t| !t.is_null()) { v["tracks"] = t.clone(); }
            v
        }
        "klipp_freesound_sign_in" => {
            let mut v = json!({ "method": "freesound_sign_in", "sign_out": args.get("sign_out").and_then(Value::as_bool).unwrap_or(false) });
            if let Some(c) = args.get("code").and_then(Value::as_str) { v["code"] = json!(c); }
            v
        }
        "klipp_style_profile" => json!({ "method": "style_profile", "media_id": arg_str(args, "media_id")? }),
        "klipp_match_grade" => {
            let mut v = json!({ "method": "match_grade", "clip_id": arg_str(args, "clip_id")?, "reference_clip_id": arg_str(args, "reference_clip_id")? });
            for k in ["secs", "reference_secs"] {
                if let Some(x) = args.get(k).and_then(|v| v.as_f64()) { v[k] = json!(x); }
            }
            v
        }
        "klipp_align_audio" => {
            let mut v = json!({ "method": "align_audio", "clip_id": arg_str(args, "clip_id")?, "reference_clip_id": arg_str(args, "reference_clip_id")? });
            if let Some(x) = args.get("max_offset_secs").and_then(|v| v.as_f64()) { v["max_offset_secs"] = json!(x); }
            v
        }
        "klipp_fonts" => json!({ "method": "fonts" }),
        "klipp_title_templates" => json!({ "method": "title_templates" }),
        "klipp_graph" | "klipp_graph_read" => {
            let mut v = args.clone();
            if !v.is_object() { return Err("graph arguments must be an object".into()); }
            let method = if name == "klipp_graph" { "graph_ops" } else { "graph_read" };
            klipp_control::graph_wire::validate_fields(&v, method, false)?;
            v["method"] = json!(method);
            if name == "klipp_graph" {
                let ops = v["ops"].as_array().filter(|ops| !ops.is_empty() && ops.len() <= 64).ok_or("a graph batch needs 1 to 64 operations")?;
                for (op_index, op) in ops.iter().enumerate() {
                    serde_json::from_value::<klipp_control::graph_wire::GraphWireOp>(op.clone())
                        .map_err(|e| json!({"op_index":op_index,"why":e.to_string(),"applied":false}).to_string())?;
                }
            }
            serde_json::from_value::<klipp_control::protocol::ControlCommand>(v.clone()).map_err(|e| format!("invalid graph request: {e}"))?;
            v
        }
        "klipp_import_timeline" => json!({"method":"import_timeline","path":args.get("path"),"new_project":args.get("new_project").and_then(Value::as_bool).unwrap_or(false)}),
        "klipp_node_types" => {
            let mut v = json!({ "method": "node_types" });
            if let Some(c) = args.get("category").and_then(|v| v.as_str()) { v["category"] = json!(c); }
            v
        }
        "klipp_looks" => {
            let mut v = json!({ "method": "looks" });
            if let Some(c) = args.get("category").and_then(|v| v.as_str()) { v["category"] = json!(c); }
            v
        }
        "klipp_look" => {
            let mut v = json!({ "method": "look", "remove": args.get("remove").and_then(|v| v.as_bool()).unwrap_or(false) });
            if let Some(c) = args.get("clip_id").and_then(|v| v.as_str()) { v["clip_id"] = json!(c); }
            if let Some(on) = args.get("on").filter(|v| !v.is_null()) { v["on"] = on.clone(); }
            if let Some(n) = args.get("name").and_then(|v| v.as_str()) { v["name"] = json!(n); }
            if let Some(o) = args.get("opacity").and_then(|v| v.as_f64()) { v["opacity"] = json!(o); }
            v
        }
        "klipp_export_status" => json!({ "method": "export_status", "wait": args.get("wait").and_then(Value::as_bool).unwrap_or(false), "job_id": args.get("job_id"), "timeout_ms": args.get("timeout_ms") }),
        "klipp_export_subtitles" => {
            let mut v = json!({ "method": "export_subtitles", "path": arg_str(args, "path")? });
            if let Some(f) = args.get("format").and_then(Value::as_str) {
                v["format"] = json!(f);
            }
            v
        }
        "klipp_import_subtitles" => json!({ "method": "import_subtitles", "path": arg_str(args, "path")? }),
        "klipp_translate_captions" => {
            let mut v = json!({ "method": "translate_captions", "to": arg_str(args, "to")?, "download_models": args.get("download_models").and_then(Value::as_bool).unwrap_or(false) });
            for key in ["track", "from"] {
                if let Some(x) = args.get(key).filter(|x| !x.is_null()) {
                    v[key] = x.clone();
                }
            }
            v
        }
        "klipp_orient" => json!({ "method": "orient" }),
        "klipp_brief" => json!({ "method": "brief" }),
        "klipp_set_brief" => {
            let brief = args.get("brief").cloned().ok_or("missing required argument: brief")?;
            // A merge flag that is not a boolean is refused: read as
            // false, "true" as a string would replace the whole brief.
            let merge = match args.get("merge") {
                None | Some(serde_json::Value::Null) => false,
                Some(v) => v.as_bool().ok_or("merge is true or false")?,
            };
            if merge {
                json!({ "method": "update_brief", "fields": brief })
            } else {
                json!({ "method": "set_brief", "brief": brief })
            }
        }
        "klipp_edit_plan" => json!({ "method": "edit_plan" }),
        "klipp_set_edit_plan" => json!({ "method": "set_edit_plan", "plan": args.get("plan").cloned().ok_or("missing required argument: plan")? }),
        "klipp_approve_edit_plan" => json!({ "method": "approve_edit_plan" }),
        "klipp_build_edit_plan" => json!({
            "method": "build_edit_plan",
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "detail": args.get("detail").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_check" => json!({ "method": "check", "color": args.get("color").and_then(|v| v.as_bool()).unwrap_or(false), "loudness": args.get("loudness").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_rename_sequence" => json!({ "method": "rename_sequence", "name": arg_str(args, "name")? }),
        "klipp_review_finding" => json!({ "method": "review_finding", "finding_id": arg_str(args, "finding_id")?, "decision": arg_str(args, "decision")?, "rationale": args.get("rationale").and_then(Value::as_str) }),
        "klipp_switch_sequence" => match (args.get("index").and_then(Value::as_u64), args.get("sequence_id").and_then(Value::as_str)) {
            (Some(index), None) => json!({ "method": "switch_sequence", "index": index }),
            (None, Some(id)) => json!({ "method": "switch_sequence", "sequence_id": id }),
            _ => return Err("give index or sequence_id (one of them), as klipp_status lists the sequences".into()),
        },
        "klipp_add_marker" => {
            let mut v = json!({ "method": "add_marker", "at_secs": args.get("at_secs").and_then(|v| v.as_f64()).ok_or("missing required argument: at_secs")? });
            for key in ["duration_secs", "label", "note", "color", "kind"] {
                if let Some(x) = args.get(key) {
                    v[key] = x.clone();
                }
            }
            v
        }
        "klipp_list_markers" => json!({ "method": "list_markers" }),
        "klipp_update_marker" => {
            let mut v = json!({ "method": "update_marker", "marker_id": arg_str(args, "marker_id")? });
            for key in ["label", "note", "color", "kind"] {
                if let Some(x) = args.get(key) {
                    v[key] = x.clone();
                }
            }
            v
        }
        "klipp_remove_marker" => json!({ "method": "remove_marker", "marker_id": arg_str(args, "marker_id")? }),
        "klipp_project_templates" => {
            let mut v = json!({ "method": "project_templates" });
            if let Some(save) = args.get("save") {
                v["save"] = save.clone();
            }
            v
        }
        "klipp_new_project" => {
            let mut v = json!({ "method": "new_project", "name": arg_str(args, "name")?, "location": arg_str(args, "location")? });
            for key in ["width", "height", "fps", "sequence_name", "discard_changes", "template"] {
                if let Some(x) = args.get(key) {
                    v[key] = x.clone();
                }
            }
            v
        }
        "klipp_new_sequence" => json!({
            "method": "new_sequence",
            "name": arg_str(args, "name")?,
            "width": args.get("width").and_then(|v| v.as_u64()).ok_or("missing required argument: width")?,
            "height": args.get("height").and_then(|v| v.as_u64()).ok_or("missing required argument: height")?,
            "fps": args.get("fps").and_then(|v| v.as_f64()).ok_or("missing required argument: fps")?,
        }),
        "klipp_create_multicam" => {
            let ids = args.get("media_ids").and_then(|v| v.as_array()).ok_or("missing required argument: media_ids")?;
            let mut v = json!({ "method": "create_multicam", "media_ids": ids });
            if let Some(sync) = args.get("sync").and_then(|v| v.as_str()) {
                v["sync"] = json!(sync);
            }
            v
        }
        "klipp_switch_angle" => json!({
            "method": "switch_angle",
            "angle": args.get("angle").and_then(|v| v.as_u64()).ok_or("missing required argument: angle")?,
            "at_secs": args.get("at_secs").and_then(|v| v.as_f64()),
        }),
        "klipp_set_multicam_sound" => json!({
            "method": "set_multicam_sound",
            "sound": match args.get("sound") {
                Some(serde_json::Value::Number(n)) => n.to_string(),
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => return Err("missing required argument: sound".into()),
            },
            "at_secs": args.get("at_secs").and_then(|v| v.as_f64()),
        }),
        "klipp_save_project" => {
            let mut v = json!({ "method": "save_project" });
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                v["path"] = json!(p);
            }
            v
        }
        "klipp_relink_media" => json!({
            "method": "relink_media",
            "media_id": arg_str(args, "media_id")?,
            "path": arg_str(args, "path")?,
            "force": args.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_tag_media" => json!({
            "method": "tag_media",
            "media_id": arg_str(args, "media_id")?,
            "add": args.get("add").cloned().unwrap_or(json!([])),
            "remove": args.get("remove").cloned().unwrap_or(json!([])),
        }),
        "klipp_window_screenshot" => json!({
            "method": "window_screenshot",
            "path": args.get("path").and_then(|v| v.as_str()),
            "max_width": args.get("max_width").and_then(|v| v.as_u64()),
        }),
        "klipp_open_project" => project_switch_command("open_project",args)?,
        "klipp_recent_projects" => json!({"method":"recent_projects"}),
        "klipp_close_project" => project_switch_command("close_project",args)?,
        "klipp_frame_stats" => json!({ "method": "frame_stats", "secs": args.get("secs").and_then(|v| v.as_f64()).ok_or("missing required argument: secs")?, "timeout_ms": args.get("timeout_ms").and_then(|v| v.as_u64()) }),
        "klipp_scope" => {
            let mut v = json!({ "method": "scope", "mode": args.get("mode").and_then(|v| v.as_str()).unwrap_or("waveform") });
            if let Some(x) = args.get("secs").and_then(|v| v.as_f64()) { v["secs"] = json!(x); }
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) { v["path"] = json!(p); }
            if let Some(t) = args.get("timeout_ms").and_then(|v| v.as_u64()) { v["timeout_ms"] = json!(t); }
            v
        }
        "klipp_timeline_sheet" => {
            let mut v = json!({ "method": "timeline_sheet" });
            for key in ["from_secs", "to_secs", "frames", "cols", "tile_width", "path", "inline"] {
                if let Some(x) = args.get(key).filter(|x| !x.is_null()) {
                    v[key] = x.clone();
                }
            }
            v
        }
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
    };
    if let Some(key) = args.get("idempotency_key") {
        let key = key.as_str().filter(|key| !key.is_empty() && key.len() <= 256)
            .ok_or("idempotency_key must be a nonempty string of at most 256 bytes")?;
        command["idempotency_key"] = json!(key);
    }
    Ok(command)
}

/// How to work here, the craft, and the recipes: files shipped with
/// the app that a user can read and change, served to the agent as
/// text. Not a prompt hidden in code.
const GUIDE: &str = include_str!("../../../assets/craft/guide.md");
const GRAPH_GUIDE: &str = include_str!("../../../assets/craft/graph.md");
const CRAFT: &str = include_str!("../../../assets/craft/reference.md");
const RECIPES: &str = include_str!("../../../assets/craft/recipes.md");
/// What a person can do here, for answering them when they ask (#626).
const ABILITIES: &str = include_str!("../../../assets/craft/abilities.md");

/// More on the editorial ops klipp_plan and klipp_apply take, after the
/// guide's own list of them (#1667).
const OPS: &str = include_str!("../../../assets/craft/ops.md");
/// Each tool's details, by area, a `## klipp_<tool>` section a tool
/// (#1667): what its short description leaves out.
const TOOL_PAGES: &[(&str, &str)] = &[
    ("session-tools", include_str!("../../../assets/craft/tools/session.md")),
    ("timeline-tools", include_str!("../../../assets/craft/tools/timeline.md")),
    ("analysis-tools", include_str!("../../../assets/craft/tools/analysis.md")),
    ("colour-tools", include_str!("../../../assets/craft/tools/colour.md")),
    ("sound-tools", include_str!("../../../assets/craft/tools/sound.md")),
];

/// A guide topic's text: a page, the ops, an area's tool page, or one
/// tool's section of it by the tool's name (`find_shots` or
/// `klipp_find_shots`).
fn guide_topic(topic: &str) -> Result<String, String> {
    Ok(match topic {
        "guide" => GUIDE.to_string(),
        "graph" => GRAPH_GUIDE.to_string(),
        "craft" => CRAFT.to_string(),
        "recipes" => RECIPES.to_string(),
        "abilities" => ABILITIES.to_string(),
        // The guide's op list, then the fuller notes on some, then every
        // op's name: the ops klipp_plan previews and klipp_apply applies.
        "ops" => {
            let list = GUIDE.find("\n## Ops\n").map_or("", |at| {
                let rest = &GUIDE[at + 1..];
                &rest[..rest[1..].find("\n## ").map_or(rest.len(), |e| e + 2)]
            });
            format!("{list}\n{OPS}\nEvery op: {}.\n", klipp_document::ops::OP_NAMES.join(", "))
        }
        "all" => format!("{GUIDE}\n\n---\n\n{CRAFT}\n\n---\n\n{RECIPES}\n\n---\n\n{GRAPH_GUIDE}"),
        other => {
            if let Some((_, page)) = TOOL_PAGES.iter().find(|(name, _)| *name == other) {
                return Ok(page.to_string());
            }
            let heading = format!("## klipp_{}\n", other.strip_prefix("klipp_").unwrap_or(other));
            let section = TOOL_PAGES.iter().find_map(|(_, page)| {
                let at = page.find(&heading)?;
                let rest = &page[at + heading.len()..];
                Some(format!("{}{}", heading, &rest[..rest.find("\n## ").map_or(rest.len(), |e| e + 1)]))
            });
            // The export's delivery presets, listed live from the list the
            // export takes them from (#1899): too long for the catalog.
            let section = section.map(|text| if heading == "## klipp_export\n" { format!("{text}\n{}\n", export_presets_line()) } else { text });
            // A tool with no section of its own is answered from the
            // catalog: what it does and what it takes, so no tool is
            // missing from the guide (#1898).
            return section.or_else(|| tool_reference(other)).ok_or_else(|| {
                let pages: Vec<&str> = TOOL_PAGES.iter().map(|(n, _)| *n).collect();
                format!("unknown topic {other}; use guide, graph, craft, recipes, abilities, ops, all, {}, or a tool's name", pages.join(", "))
            });
        }
    })
}

/// A tool's entry in the guide, made from its catalog entry (#1898): its
/// description, then each argument with its type, whether it is needed,
/// its description and the values it takes. `None` for no such tool.
fn tool_reference(name: &str) -> Option<String> {
    let full = format!("klipp_{}", name.strip_prefix("klipp_").unwrap_or(name));
    let tools = tool_definitions_for(false);
    let tool = tools.iter().find(|t| t["name"] == full.as_str())?;
    let mut out = format!("## {full}\n\n{}\n", tool["description"].as_str().unwrap_or(""));
    match tool["inputSchema"]["properties"].as_object().filter(|p| !p.is_empty()) {
        None => out.push_str("\nIt takes no arguments.\n"),
        Some(properties) => {
            out.push_str("\nArguments:\n");
            describe_arguments(properties, &tool["inputSchema"]["required"], 0, &mut out);
        }
    }
    // Said, so a reader looking for more knows there is none yet (#1898).
    out.push_str("\nNo guide section yet: this is the tool's catalog entry, made from its schema.\n");
    Some(out)
}

/// Each argument in `properties`: its type, whether `required` names it,
/// its description and the values it takes; an object's fields, or an
/// array's items' fields, one level down, and said when they aren't
/// listed.
fn describe_arguments(properties: &serde_json::Map<String, Value>, required: &Value, depth: usize, out: &mut String) {
    let required: Vec<&str> = required.as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let pad = "  ".repeat(depth);
    for (arg, spec) in properties {
        let kind = spec["type"].as_str().unwrap_or("any");
        let need = if required.contains(&arg.as_str()) { ", required" } else { "" };
        out.push_str(&format!("{pad}- `{arg}` ({kind}{need})"));
        if let Some(d) = spec["description"].as_str() {
            out.push_str(&format!(": {d}"));
        }
        if let Some(values) = spec["enum"].as_array() {
            let values: Vec<String> = values.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).collect();
            out.push_str(&format!(" One of: {}.", values.join(", ")));
        }
        out.push('\n');
        // What is inside an object or an array of objects.
        let (inner, label) = match kind {
            "object" => (spec, "its fields"),
            "array" => (&spec["items"], "each item's fields"),
            _ => continue,
        };
        match inner["properties"].as_object().filter(|p| !p.is_empty()) {
            Some(fields) if depth == 0 => {
                out.push_str(&format!("{pad}  {label}:\n"));
                describe_arguments(fields, &inner["required"], depth + 2, out);
            }
            _ if inner["type"].as_str().is_some_and(|t| t != "object") => {}
            _ => out.push_str(&format!("{pad}  ({label} are not listed here; the tool's description and its reply say what it takes)\n")),
        }
    }
}

/// `klipp_export`'s `preset`: a built-in delivery preset by name, each
/// with the container its path must end in (#1899).
fn export_preset_schema() -> Value {
    json!({
        "type": "string",
        "description": "Delivery preset: klipp_guide topic export",
    })
}

/// The delivery presets `preset` takes, each with its container.
fn export_presets_line() -> String {
    let listed: Vec<String> = klipp_control::export_presets().into_iter().map(|(n, ext)| format!("{n} (.{ext})")).collect();
    format!("The delivery presets: {}.", listed.join(", "))
}

/// Sent with `initialize`, which clients put before the model in every
/// session: how to answer the two things a person asks first (#626).
const INSTRUCTIONS: &str = "Klipp is a video editor running on this computer; these tools work in the open project.

When the person asks whether Klipp is connected, open or working: call klipp_status (it reads and changes nothing) and answer from its text, in plain words: yes or no first, then the version, the open project and whether it is saved, the timeline, and the playhead, then say that nothing was changed. No socket paths, ids or JSON. If the call fails, say Klipp does not seem to be running and to start it and ask again.

When the person asks what they can do with Klipp, what you can do in it, or how to start: read klipp_guide with topic abilities and answer from it, short and in their words: a sentence on what working together looks like, the groups that fit with an example request each, then one first step that fits what is open (read with klipp_status), and last, that there is more, naming two or three other areas they can ask about. No tool names.

Before editing, read klipp_guide (topic guide) for how to work here.";

fn call_tool(name: &str, args: &Value) -> Result<(Vec<Value>, Option<Value>), String> {
    if name == "klipp_guide" {
        let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("guide");
        let text = guide_topic(topic)?;
        return Ok((vec![json!({ "type": "text", "text": text })], None));
    }
    if !diagnostics::tool_names().iter().any(|known| known == name) {
        return Err(format!("This MCP server ({}) does not advertise {name}. Reconnect the Klipp MCP connection in your client and refresh its tool list; restarting the Klipp app alone does not refresh this server.", diagnostics::label()));
    }
    let mut cmd = tool_to_command(name, args)?;
    if (WRITE_TOOLS.contains(&name) || RETRYABLE_TOOLS.contains(&name))
        && cmd.get("idempotency_key").is_none()
    {
        cmd["idempotency_key"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let mut result = control_call(cmd)?;
    if matches!(name, "klipp_import_timeline" | "klipp_status" | "klipp_orient") {
        diagnostics::attach(&mut result, args.get("expected_catalog_fingerprint").and_then(Value::as_str));
    }
    // The project changed under this session — made new, opened or saved
    // as — since its last call: said, so what it knew about the timeline
    // and the pool is not trusted a moment longer.
    // By the project's identity, which a save where it already was does
    // not move; the generation moves on every save, so a Ctrl+S in the
    // app used to raise this for nothing.
    let mut notice: Option<String> = None;
    if let Some(g) = result.get("project_identity").or_else(|| result.get("project_generation")).and_then(|v| v.as_u64()) {
        let seen = LAST_PROJECT_GENERATION.swap(g, std::sync::atomic::Ordering::Relaxed);
        if project_changed_under(name, seen, g) {
            notice = Some("Note: the project changed under you since your last call (a new project, or one opened or saved as another) — the timeline, the media pool and their ids are not what they were. Read klipp_status, klipp_list_media and klipp_timeline again before going on.".into());
        }
    }

    let mut content = content_for(name, &result)?;
    if let Some(n) = notice {
        content.insert(0, json!({ "type": "text", "text": n }));
    }
    Ok((content, Some(result)))
}

/// Whether a call's reply says the project changed under the session:
/// the identity moved since the last call, and the call was not the one
/// that changed it (a new project, an open, a save as; #1574).
fn project_changed_under(name: &str, seen: u64, now: u64) -> bool {
    seen != u64::MAX
        && seen != now
        && !matches!(name, "klipp_new_project" | "klipp_open_project" | "klipp_close_project" | "klipp_save_project")
}

fn has_structured_output(name: &str) -> bool {
    matches!(name, "klipp_import_timeline" | "klipp_status" | "klipp_orient" | "klipp_timeline" | "klipp_plan" | "klipp_apply" | "klipp_prepare_cut" | "klipp_grade_to_target" | "klipp_check" | "klipp_review")
}

fn output_schema(name: &str) -> Value {
    if name == "klipp_import_timeline" {
        return json!({"type":"object","properties":{"report":{"type":["object","null"]},"request_id":{"type":"string"},"error":{"type":"string"}},"additionalProperties":true});
    }
    let mut properties = json!({
        "text": {"type":"string"},
        "revision": {"type":"integer","minimum":0},
        "project_identity": {"type":"integer","minimum":0},
        "applied": {"type":"boolean"},
        "changes": {"type":"array"},
        "candidates": {"type":"array"}
    });
    if name == "klipp_status" { properties["version"] = json!({"type":"string"}); }
    json!({"type":"object","properties":properties,"additionalProperties":true})
}

/// A reply's content: the frame-wait bookkeeping for writes, text for
/// the views that are text, pictures and sound inlined when small.
fn content_for(name: &str, result: &Value) -> Result<Vec<Value>, String> {
    let result = result.clone();
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
    // The next-step hint, kept in the text views where the JSON is not
    // shown: it is the whole point of the hint that the agent reads it.
    let with_next = |mut text: String| -> Vec<Value> {
        if let Some(n) = result.get("next").and_then(|v| v.as_str()) {
            text.push_str(&format!("\n\nNext: {n}"));
        }
        if let Some(n) = result.get("note").and_then(|v| v.as_str()) {
            text.push_str(&format!("\n\nNote: {n}"));
        }
        vec![json!({ "type": "text", "text": text })]
    };
    // Music/SFX search: the readable list PLUS the machine-usable fields,
    // because add_music needs a result's URLs and the summary text hides
    // them. Without this the search→add workflow cannot complete.
    if name == "klipp_search_music" || name == "klipp_search_sfx" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let mut out = text.to_string();
            if let Some(results) = result.get("results") {
                out.push_str("\n\nTo add one, pass its fields to klipp_add_music. Results:\n");
                out.push_str(&serde_json::to_string_pretty(results).unwrap_or_default());
            }
            return Ok(with_next(out));
        }
    }
    // Beats: the readable line PLUS the beat_times array, so the agent can
    // snap cuts to the actual beats (the summary text hides the grid).
    if name == "klipp_beats" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let mut out = text.to_string();
            if let Some(bt) = result.get("beat_times") {
                out.push_str("\n\nCut or place on one of these beat times (seconds):\n");
                out.push_str(&serde_json::to_string(bt).unwrap_or_default());
            }
            // The fit's ops, to hand to klipp_apply as they are (#503).
            if let Some(ops) = result.get("ops") {
                out.push_str("\n\nOps to apply:\n");
                out.push_str(&serde_json::to_string(ops).unwrap_or_default());
            }
            return Ok(with_next(out));
        }
    }
    // Beat sheet: the readable summary PLUS the machine-usable cut points
    // and title/hold windows, so the agent can place cuts and titles on
    // exact times instead of guessing.
    if name == "klipp_beat_sheet" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let mut out = text.to_string();
            for (label, key) in [("Cut points", "cut_points"), ("Title / hold windows", "title_windows"), ("Beats with labels", "beats")] {
                if let Some(arr) = result.get(key) {
                    out.push_str(&format!("\n\n{label} (JSON):\n"));
                    out.push_str(&serde_json::to_string(arr).unwrap_or_default());
                }
            }
            return Ok(with_next(out));
        }
    }
    // The plain summary first, as a person is told it; the fields after
    // for the agent's own use (#626).
    if name == "klipp_status" || name == "klipp_job_status" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let mut rest = result.clone();
            if let Some(o) = rest.as_object_mut() {
                o.remove("text");
            }
            let out = format!("{text}\n\nDetails:\n{}", serde_json::to_string_pretty(&rest).unwrap_or_default());
            return Ok(vec![json!({ "type": "text", "text": out })]);
        }
    }
    if name == "klipp_plan" {
        if let Some(summary) = result.get("text").and_then(|v| v.as_str()) {
            let mut out = summary.to_string();
            if let (Some(revision), Some(project)) = (
                result.get("revision").and_then(Value::as_u64),
                result.get("project_identity").and_then(Value::as_u64),
            ) {
                out.push_str(&format!("\n\nTo apply this plan, pass expected_revision: {revision} and expected_project_identity: {project} with the same ops."));
            }
            return Ok(with_next(out));
        }
    }
    if name == "klipp_generator" || name == "klipp_timeline" || name == "klipp_media_analysis" || name == "klipp_plan" || name == "klipp_check" || name == "klipp_brief" || name == "klipp_edit_plan" || name == "klipp_build_edit_plan" || name == "klipp_screen_text" || name == "klipp_auto_redact" || name == "klipp_auto_sfx" || name == "klipp_review" || name == "klipp_grade_to_target" || name == "klipp_measure_region" || name == "klipp_colour_selfcheck" || name == "klipp_grade_chain" || name == "klipp_match_scenes" || name == "klipp_adopt_look" || name == "klipp_audio_spectrum" || name == "klipp_skin_balance" || name == "klipp_transcript" || name == "klipp_cut_points" || name == "klipp_selects" || name == "klipp_style_profile" || name == "klipp_looks" || name == "klipp_frame_stats" {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let mut text = text.to_string();
            // Text-only clients need the same review handles as structured
            // clients; otherwise the next suggested action is impossible.
            if matches!(name, "klipp_check" | "klipp_review") {
                let check = if name == "klipp_review" { &result["check"] } else { &result };
                for key in ["findings", "color_findings", "sound_findings"] {
                    if let Some(findings) = check[key].as_array() {
                        for finding in findings {
                            if let Some(id) = finding["id"].as_str() {
                                text.push_str(&format!("\nFinding {id}: {}", finding["what"].as_str().unwrap_or("")));
                            }
                        }
                    }
                }
            }
            // A grade pass writes many times: the next edit needs the
            // revision it ended at, not the one before it (#1571).
            if name == "klipp_grade_to_target" {
                if let Some(revision) = result.get("revision").and_then(Value::as_u64) {
                    text.push_str(&format!("\n\nRevision: {revision}"));
                }
            }
            return Ok(with_next(text));
        }
    }
    // An apply says what it did as the plan would, and that it is done:
    // the steps as JSON on top of that cost a long assembly's caller
    // fifteen thousand tokens. `detail` asks for them.
    if name == "klipp_apply" && result.get("steps").is_none() {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            let outcome = if result.get("applied").and_then(Value::as_bool) == Some(true) {
                format!("Applied as one undo step; revision {}. Operation {}.",
                    result.get("revision").and_then(Value::as_u64).unwrap_or(0),
                    result.get("operation_id").and_then(Value::as_str).unwrap_or("unknown"))
            } else {
                format!("Queued for Klipp's UI; final outcome is not known yet. Operation {}. Call klipp_operation_status with that ID before retrying or continuing.",
                    result.get("operation_id").and_then(Value::as_str).unwrap_or("unknown"))
            };
            let mut message = format!("{text}{outcome}");
            if result.get("applied").and_then(Value::as_bool) == Some(false) {
                return Ok(vec![json!({ "type": "text", "text": message })]);
            }
            if let Some(changes) = result.get("changes") {
                message.push_str("\nExact cut changes (before and after, sequence frames):\n");
                message.push_str(&serde_json::to_string(changes).unwrap_or_default());
            }
            if let Some(shifted) = result.get("shifted") {
                message.push_str("\nClips shifted by ripple (count per track and frame offset):\n");
                message.push_str(&serde_json::to_string(shifted).unwrap_or_default());
            }
            if let Some(moved) = result.get("retracked").and_then(Value::as_array).filter(|m| !m.is_empty()) {
                message.push_str("\nClips that only moved track index (count per move):\n");
                message.push_str(&serde_json::to_string(moved).unwrap_or_default());
            }
            return Ok(with_next(message));
        }
    }
    if name == "klipp_list_clips" {
        return Ok(vec![json!({ "type": "text", "text": clips_text(&result) })]);
    }
    // Pictures and sound come back as a path; inline them too when
    // small enough to be worth a context window.
    let inline = match name {
        "klipp_screenshot" | "klipp_frame_at" | "klipp_scope" | "klipp_timeline_sheet" => Some(("image", "image/png")),
        "klipp_contact_sheet" => Some(("image", "image/jpeg")),
        "klipp_audio_excerpt" => Some(("audio", "audio/wav")),
        _ => None,
    };
    if let Some((kind, mime)) = inline {
        if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
            let mut content = text_content(&result);
            // A sheet written where it was asked stays there (#796).
            if result.get("inline").and_then(Value::as_bool) == Some(false) {
                return Ok(content);
            }
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

fn tool(name: &str, description: &str, mut properties: Value, required: Vec<&str>) -> Value {
    if WRITE_TOOLS.contains(&name) || RETRYABLE_TOOLS.contains(&name) {
        // What it is, said once, in klipp_guide's description: here it
        // was the same sentence on 62 tools (#1984).
        properties["idempotency_key"] = json!({ "type": "string" });
    }
    // A tool says what it does in a sentence or two and names the guide
    // topic its details are in (#1667); where those details are an
    // argument's format, the argument names the topic too.
    if let Some(field) = match name {
        "klipp_generator" => Some("data"),
        "klipp_set_brief" => Some("brief"),
        "klipp_set_edit_plan" => Some("plan"),
        "klipp_narrate" => Some("voice"),
        _ => None,
    } {
        properties[field]["description"] = json!(format!("Its format: klipp_guide topic {}.", &name["klipp_".len()..]));
    }
    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
    // The common ops' strict shapes once, on the preview; the apply takes
    // the same ops (#1667).
    if name == "klipp_plan" {
        properties["ops"] = edit_schema::ops_schema();
    } else if name == "klipp_apply" {
        properties["ops"] = json!({"type":"array","minItems":1,"items":{"type":"object","required":["op"]},"description":"The ops of klipp_plan's schema; every op: klipp_guide topic ops."});
    }
    let mut schema=json!({"type":"object","properties":properties});
    // An omitted required list means no required arguments. Avoid paying
    // for empty arrays on every read/optional-argument tool in the catalog.
    if !required.is_empty() { schema["required"]=json!(required); }
    json!({ "name":name, "description":description, "inputSchema":schema })
}

#[cfg(test)]
fn tool_definitions() -> Vec<Value> { tool_definitions_for(false) }

fn tool_definitions_for(structured: bool) -> Vec<Value> {
    let mut tools = vec![
        tool("klipp_guide",
             "Read this first. How to work in Klipp as an agent (the loop: read, learn the \
              footage, plan, apply, look, deliver; units, ids, anchors, ops, manners), the \
              craft reference (what a professional does without being asked), and the \
              recipes (worked plans for an interview with B-roll, a social cut-down, a \
              montage). Files the user can read and change. A tool's details are under its \
              name as the topic. Every write takes an optional idempotency_key: on a lost reply, \
              retry the identical call with the same key (kept until Klipp exits).",
             json!({ "topic": { "type": "string", "description": "guide (default), graph (read before graph edits), craft, recipes, abilities (what a person can do here, for answering them when they ask), ops (every edit op), all, an area (session-tools, timeline-tools, analysis-tools, colour-tools, sound-tools), or a tool's name (find_shots) for its details" } }),
             vec![]),
        tool("klipp_status",
             "App status, MCP build and catalog mismatch warnings. Read-only. Details: klipp_guide topic status.",
             json!({ "expected_catalog_fingerprint": { "type": "string", "description": "Fingerprint from tools/list _meta; warns on mismatch." } }), vec![]),
        tool("klipp_operation_status",
             "The outcome of a queued write by its operation_id: waiting, applied or refused, with the final revision and the reason. Details: klipp_guide topic operation_status.",
             json!({ "operation_id": { "type": "string" } }), vec!["operation_id"]),
        tool("klipp_job_status",
             "Live render and analysis jobs in one shape: phase, progress, result and error; job_id for one job, wait true to long-poll. Details: klipp_guide topic job_status.",
             json!({ "job_id": { "type": "string" }, "wait": { "type": "boolean" },
                     "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 10000 } }), vec![]),
        tool("klipp_follow_job",
             "Wait for one render or analysis job to end and say how it ended, with the way there. Details: klipp_guide topic follow_job.",
             json!({ "job_id": { "type": "string" },
                     "max_wait_ms": { "type": "integer", "minimum": 1, "maximum": 3600000 } }), vec!["job_id"]),
        tool("klipp_cancel_job",
             "Cancel a render, a queued analysis job or a subject matte pass; other running analysis, import and sync cannot be stopped. Details: klipp_guide topic cancel_job.",
             json!({ "job_id": { "type": "string" } }), vec!["job_id"]),
        tool("klipp_play", "Start playback.", json!({}), vec![]),
        tool("klipp_pause", "Pause playback.", json!({}), vec![]),
        tool("klipp_stop", "Stop playback and return to the start.", json!({}), vec![]),
        tool("klipp_seek",
             "Move the playhead to an absolute position, in seconds from the start \
              of the timeline.",
             json!({ "secs": { "type": "number", "description": "Seconds from timeline start" } }),
             vec!["secs"]),
        tool("klipp_list_clips",
             "Every track and its clips, a line a clip: ids, kinds, ranges, names, media ids and transitions; narrow by track, time or media. Clip ids here are what the grade tools take. Details: klipp_guide topic list_clips.",
             json!({
                 "track": { "type": ["integer", "string"], "description": "A track's index or name" },
                 "from_secs": { "type": "number", "description": "Only clips that end after this" },
                 "to_secs": { "type": "number", "description": "Only clips that start before this" },
                 "media_id": { "type": "string", "description": "Only clips of this media item" },
                 "full": { "type": "boolean", "description": "Every field (default false)" },
             }),
             vec![]),
        tool("klipp_list_media",
             "The media pool: media ids, paths, kind, duration, fps, resolution, codec. Details: klipp_guide topic list_media.",
             json!({}), vec![]),
        tool("klipp_generator",
             "Preview a native generator clip; apply true places it as one undo step. Give at_secs and secs; data describes the native generator. Overlaps and locked tracks are refused. Details: klipp_guide topic generator.",
             json!({ "data": { "type": ["object", "string"] }, "at_secs": { "type": "number" }, "secs": { "type": "number" }, "track": { "type": "integer" }, "apply": { "type": "boolean" } }), vec!["data", "at_secs", "secs"]),
        tool("klipp_import_media",
             "Import media: one path, several paths, or a folder's media files. Each is answered \
              with its media_id or error; no media_id yet means still probing: follow its job_id.",
             json!({ "path": { "type": "string" }, "paths": { "type": "array", "items": { "type": "string" } }, "folder": { "type": "string" }, "analyze": { "type": "boolean", "description": "default true" } }),
             vec![]),
        tool("klipp_append_clip",
             "Append a media pool item to the timeline as a new clip. A clip at a different \
              frame rate from the project is added without changing the project's rate.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media" },
                 "track": { "type": "integer", "description": "Track index; omit for the default track" },
             }),
             vec!["media_id"]),
        tool("klipp_set_append_audio",
             "Source patching: whether appending a clip also brings its audio (on for camera footage, off for cutting to music). Details: klipp_guide topic set_append_audio.",
             json!({ "enabled": { "type": "boolean", "description": "true brings audio, false is video only" } }),
             vec!["enabled"]),
        tool("klipp_attach_audio",
             "Give a clip its own media file's audio, linked and spanning the clip; refused if it has it already. Details: klipp_guide topic attach_audio.",
             json!({ "clip_id": { "type": "string", "description": "From klipp_list_clips" } }),
             vec!["clip_id"]),
        tool("klipp_normalize",
             "Bring clips to a loudness or peak target by their gain, as one undo step; the media is not changed. Details: klipp_guide topic normalize.",
             json!({
                 "clip_ids": { "type": "array", "items": { "type": "string" }, "description": "From klipp_list_clips" },
                 "mode": { "type": "string", "enum": ["loudness", "peak"], "description": "loudness: integrated LUFS (ITU-R BS.1770); peak: sample peak dBFS. Default loudness" },
                 "target": { "type": "number", "description": "LUFS or dBFS. Default -14 LUFS (web) or -1 dBFS; -16 podcast, -23 EBU R128, -24 ATSC A/85" },
                 "ceiling_dbtp": { "type": "number", "description": "True-peak ceiling, default -1 dBTP" },
                 "group": { "type": "boolean", "description": "One gain for all the clips. Default false" },
                 "apply": { "type": "boolean", "description": "false answers what it would set. Default true" }
             }),
             vec!["clip_ids"]),
        tool("klipp_remove_clip", "Remove a clip from the timeline.",
             json!({ "clip_id": { "type": "string", "description": "From klipp_list_clips" } }),
             vec!["clip_id"]),
        tool("klipp_detect_scene_cuts",
             "Split a clip at the hard cuts in its picture (a flattened movie or montage), in the background, as one undo step. Details: klipp_guide topic detect_scene_cuts.",
             json!({ "clip_id": { "type": "string", "description": "The clip to split" } }),
             vec!["clip_id"]),
        tool("klipp_track_surface",
             "Track a planar surface in perspective for the pin op, from a click or corners measured with klipp_frame_at; in the background, as one undo step. Details: klipp_guide topic track_surface.",
             json!({
                 "clip_id": { "type": "string", "description": "The clip of footage the surface is in" },
                 "click": { "type": "array", "minItems": 2, "maxItems": 2, "items": { "type": "number" }, "description": "[x,y] inside the surface, fractions of the frame; instead of corners" },
                 "corners": { "type": "array", "minItems": 4, "maxItems": 4,
                              "items": { "type": "array", "minItems": 2, "maxItems": 2, "items": { "type": "number" } },
                              "description": "[[x,y] top left, top right, bottom right, bottom left], fractions of the frame" },
                 "at_secs": { "type": "number", "description": "Timeline second the corners are on; the playhead when left out" },
                 "name": { "type": "string", "description": "What to call the surface, e.g. Phone screen" },
                 "surface": { "type": "string", "description": "Id or name of a surface to track again from these corners" },
                 "direction": { "type": "string", "enum": ["both", "forward", "backward"], "description": "One way only, merged into the surface's track; needs surface" },
             }),
             vec!["clip_id"]),
        tool("klipp_select_clip",
             "Select a clip (or pass no id to clear the selection). The inspector and \
              colour page follow the selection.",
             json!({ "clip_id": { "type": "string", "description": "Omit to deselect" } }),
             vec![]),
        tool("klipp_get_grade",
             "The clip's colour node graph as JSON, every node id with it: read it before changing a grade. Details: klipp_guide topic get_grade.",
             json!({ "clip_id": { "type": "string" } }), vec!["clip_id"]),
        tool("klipp_add_node",
             "Add an unconnected colour node (a NodeKind name, or ofx:<identifier>) to a clip's grade graph; replies with its node_id. Wire it with klipp_connect_nodes. Details: klipp_guide topic add_node.",
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
             "Wire one grade node's output into another's input port, in one graph.",
             json!({
                 "clip_id": { "type": "string" },
                 "from": { "type": "string", "description": "Source node id" },
                 "from_port": { "type": "integer" },
                 "to": { "type": "string", "description": "Destination node id" },
                 "to_port": { "type": "integer", "description": "0 primary, 1 secondary (e.g. matte)" },
             }),
             vec!["clip_id", "from", "to"]),
        tool("klipp_set_node_param",
             "Set one numeric parameter on one of a clip's grade nodes, or key it at a time to ride it over the shot; read klipp_get_grade first for its node ids. Details: klipp_guide topic set_node_param.",
             json!({
                 "clip_id": { "type": "string" },
                 "node_id": { "type": "string", "description": "From klipp_get_grade for THIS clip" },
                 "param": { "type": "string", "description": "Parameter name, e.g. temperature" },
                 "value": { "type": "number" },
                 "at": { "type": "number", "description": "Key it at this timeline second" },
                 "interpolation": { "type": "string", "enum": ["linear", "ease_in", "ease_out", "ease_both", "bezier", "hold"] },
                 "remove": { "type": "boolean", "description": "Take the key at `at` away" },
             }),
             vec!["clip_id", "node_id", "param"]),
        tool("klipp_toggle_node", "Enable or disable a grade node (a bypass toggle).",
             json!({ "clip_id": { "type": "string" }, "node_id": { "type": "string" } }),
             vec!["clip_id", "node_id"]),
        tool("klipp_rename_node", "Give a grade node a name of its own, shown on its card in the node graph: \"skin\", \"sky\", \"look\".",
             json!({ "clip_id": { "type": "string" }, "node_id": { "type": "string", "description": "From klipp_get_grade for THIS clip" }, "name": { "type": "string" } }),
             vec!["clip_id", "node_id", "name"]),
        tool("klipp_set_log_format",
             "Set the camera log or HDR encoding a clip's media was recorded in, when its metadata does not say; every clip of the file follows. Details: klipp_guide topic set_log_format.",
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
             "What the viewer shows now, as a PNG: how to see a grade's result. Details: klipp_guide topic screenshot.",
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
             "Frame timings (decode, grade, composite, drops, readbacks) and presented_fps over the last 2 s, to tell whether a change made playback faster. Details: klipp_guide topic perf.",
             json!({}), vec![]),
        tool("klipp_log_tail",
             "Tail of Klipp's debug log. The engine logs its decisions here, including \
              why the direct-present fast path did or did not engage.",
             json!({ "lines": { "type": "integer", "description": "How many lines (default 100)" } }),
             vec![]),
        tool("klipp_timeline",
             "The timeline as text: tracks, every clip with its range, kind, grade, effects and id, the transitions and the markers. Read it before planning an edit. Details: klipp_guide topic timeline.",
             json!({
                 "max_chars": { "type": "integer", "description": "Cut on a line boundary at this many characters (default 12000)" },
                 "track": { "type": "integer", "description": "One track only, by index (0 is the top)" },
                 "from_secs": { "type": "number", "description": "Only what plays from here on the timeline" },
                 "to_secs": { "type": "number", "description": "Only what plays before here" },
             }),
             vec![]),
        tool("klipp_analyze",
             "Start background analysis of the pool or one item: shots, loudness, silence, transcript, screen beats and OCR. Models download only with download_models. Details: klipp_guide topic analyze.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media; omit for every item" },
                 "wait": { "type": "boolean", "description": "Wait until all requested items end; inspect finished and succeeded" },
                 "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour; timeout does not cancel analysis" },
                 "download_models": { "type": "boolean", "description": "Fetch missing models first (the user should know a download happens)" },
             }),
             vec![]),
        tool("klipp_track_subject",
             "Track a clip's subject into grade or cut_out; returns a job_id. into whole restores its background in one undo step, without a job. Models fetched only with download_model. Details: klipp_guide topic track_subject.",
             json!({
                 "clip_id": { "type": "string", "description": "From klipp_list_clips" },
                 "transition_id": { "type": "string", "description": "In place of clip_id: a piece transition whose pieces cut round the subject" },
                 "model": { "type": "string", "enum": ["subject", "foreground"], "description": "subject: one detected subject, followed (default); foreground: the whole foreground" },
                 "download_model": { "type": "boolean", "description": "Fetch the model first when missing (the user should know a download happens)" },
                 "into": { "type": "string", "enum": ["grade", "cut_out", "whole"] },
             }),
             vec![]),
        tool("klipp_search_transcripts",
             "Find where something is said: every transcript segment containing the query, \
              case-insensitively, with its media item and its start and end in the file.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" } }),
             vec!["query"]),
        tool("klipp_screen_text",
             "What a screen recording showed and when: OCR lines folded into spans with their times and places, sensitive ones masked. Needs the OCR models. Details: klipp_guide topic screen_text.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "all": { "type": "boolean", "description": "every line read, not the summary of headings and lines that stayed up" } }),
             vec!["media_id"]),
        tool("klipp_grade_to_target",
             "Measure the picture against the brief's look in CIELAB and grade toward it, up to three rounds; apply true makes one undo step. Details: klipp_guide topic grade_to_target.",
             json!({ "clip_id": { "type": "string" }, "apply": { "type": "boolean" }, "rounds": { "type": "integer" }, "force": { "type": "boolean" }, "strength": { "type": "string", "enum": ["gentle", "normal", "bold"], "description": "how far each knob may move from where the shot started; gentle unless asked" } }), vec![]),
        tool("klipp_audio_spectrum",
             "A file's sound as frequency against time over the whole file: named findings (hum, hiss, rumble) and band levels a second at a time. Details: klipp_guide topic audio_spectrum.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" } }), vec!["media_id"]),
        tool("klipp_adopt_look",
             "Take a look's numbers off a reference picture (a pool file, or the best-graded shot) as the target colour is judged by; apply true writes it to the brief. Details: klipp_guide topic adopt_look.",
             json!({ "media_id": { "type": "string" }, "from_timeline": { "type": "boolean" }, "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_match_scenes",
             "Group the shots into scenes by their light, pick a hero in each and grade the rest onto it; apply true grades the pass as one undo step. Details: klipp_guide topic match_scenes.",
             json!({ "apply": { "type": "boolean" }, "rounds": { "type": "integer" } }), vec![]),
        tool("klipp_grade_chain",
             "A clip's grade chain in picture order, each node named with its stage, and what runs out of order. Read when a grading loop will not settle. Details: klipp_guide topic grade_chain.",
             json!({ "clip_id": { "type": "string" } }), vec![]),
        tool("klipp_colour_selfcheck",
             "Send a ColorChecker through the CPU and GPU render paths and read it back, to see whether the pipeline itself moves colour. Run once before grading to targets. Details: klipp_guide topic colour_selfcheck.",
             json!({ "size": { "type": "integer" } }), vec![]),
        tool("klipp_measure_region",
             "One box of the frame read alone, in klipp_frame_stats's numbers; x, y, w, h are fractions. Details: klipp_guide topic measure_region.",
             json!({ "secs": { "type": "number" }, "x": { "type": "number" }, "y": { "type": "number" }, "w": { "type": "number" }, "h": { "type": "number" }, "timeout_ms": { "type": "integer" } }), vec!["secs", "x", "y", "w", "h"]),
        tool("klipp_skin_balance",
             "Turn faces' skin hue onto the 40-65 degree band by measurement, with a skin secondary, up to three rounds with apply true. Details: klipp_guide topic skin_balance.",
             json!({ "clip_id": { "type": "string" }, "apply": { "type": "boolean" } }), vec!["clip_id"]),
        tool("klipp_review",
             "The cut as an editor scrubs it, in one call: a fast draft render, a contact sheet of it, and the check with its colour pass. Details: klipp_guide topic review.",
             json!({ "max_width": { "type": "integer" }, "frames": { "type": "integer" }, "cols": { "type": "integer" }, "path": { "type": "string" } }), vec![]),
        tool("klipp_narrate",
             "Create local narration and import the take; apply true places it on a Voice track. Nothing downloads unless download_models is true. Voices, licences and prerequisites: klipp_guide topic narrate.",
             json!({ "text": { "type": "string" }, "at_secs": { "type": "number" }, "voice": { "type": "string" }, "speed": { "type": "number" }, "pause_secs": { "type": "number" }, "fit_secs": { "type": "number", "description": "seconds the take must last" }, "variation": { "type": "number", "description": "Piper: 0 flat to 2" }, "download_models": { "type": "boolean" }, "apply": { "type": "boolean" }, "path": { "type": "string" } }), vec!["text"]),
        tool("klipp_record",
             "Record the screen (focused window, screen, monitor or region), imported into the pool. Details: klipp_guide topic record.",
             json!({ "secs": { "type": "number" }, "target": { "type": "string" }, "fps": { "type": "integer" }, "path": { "type": "string" }, "import": { "type": "boolean" },
                     "wait": { "type": "boolean" }, "action": { "type": "string", "enum": ["start", "status", "stop"] }, "recording_id": { "type": "string" },
                     "codec": { "type": "string" }, "quality": { "type": "string" } }), vec![]),
        tool("klipp_audition",
             "Play a file's stretch, a clip, the mix or search previews through Klipp's output, for the person to hear. Details: klipp_guide topic audition.",
             json!({ "action": { "type": "string", "enum": ["play", "stop", "status"] }, "media_id": { "type": "string" }, "clip_id": { "type": "string" }, "sequence": { "type": "boolean" },
                     "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "preview_urls": { "type": "array", "items": { "type": "string" } } }), vec![]),
        tool("klipp_mix_loudness",
             "The mix's loudness over a stretch as the export mixes it, step by step, the steps 4 dB over the median marked. Details: klipp_guide topic mix_loudness.",
             json!({ "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "step_secs": { "type": "number" } }), vec![]),
        tool("klipp_auto_sfx",
             "Preview sound design for the cut: whooshes on cuts, clicks on reveals, a riser into the last cut; apply true commits it as one undo step. Details: klipp_guide topic auto_sfx.",
             json!({ "whoosh": { "type": "string" }, "click": { "type": "string" }, "riser": { "type": "string" }, "on": { "type": "string", "enum": ["cuts", "transitions", "stylised"] }, "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_auto_redact",
             "Pixelate every sensitive thing the screen text found (emails, keys, passwords, card numbers) where the timeline shows it; a dry run unless apply is true. Details: klipp_guide topic auto_redact.",
             json!({ "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_search_screen_text",
             "Find every moment a word or phrase was on screen, across all footage. Details: \
              klipp_guide topic search_screen_text.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" } }),
             vec!["query"]),
        tool("klipp_search_visual",
             "Find footage by what its picture shows (\"a dog on a beach\"), best first.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" }, "min_score": { "type": "number" } }),
             vec!["query"]),
        tool("klipp_analysis_queue", "Put media_id first in the analysis queue, or cancel the pending items. Details: klipp_guide topic analysis_queue.",
             json!({ "media_id": { "type": "string" }, "cancel_pending": { "type": "boolean" } }), vec![]),
        tool("klipp_analysis_status", "Where analysis stands. With wait true, holds the response until the selected items end or timeout. Reports finished, succeeded and timed_out; failed items carry errors.",
             json!({ "wait": { "type": "boolean" }, "media_id": { "type": "string" }, "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour; does not cancel work" } }), vec![]),
        tool("klipp_media_analysis",
             "What is known about a media item as text: its shots, loudness, true peak and silences, in seconds of the file. Details: klipp_guide topic media_analysis.",
             json!({ "media_id": { "type": "string" }, "max_chars": { "type": "integer", "description": "Cut at this many characters (default 8000)" } }),
             vec!["media_id"]),
        tool("klipp_beat_sheet",
             "A screencast's beat sheet: idle and action stretches, cut points and title holds, named by the screen text when OCR ran. Analyze first. Details: klipp_guide topic beat_sheet.",
             json!({ "media_id": { "type": "string" } }),
             vec!["media_id"]),
        tool("klipp_find_shots",
             "Shots across every analysed item matching all conditions given: framing, people, objects, motion, speech and length; long takes searched moment by moment. Details: klipp_guide topic find_shots.",
             json!({
                 "framing": { "type": "string" }, "min_people": { "type": "integer" },
                 "object": { "type": "string" }, "motion": { "type": "string" },
                 "with_speech": { "type": "boolean" }, "min_secs": { "type": "number" },
                 "max_hits": { "type": "integer" },
             }),
             vec![]),
        tool("klipp_contact_sheet",
             "A grid of frames over a media file's range, each with its time burnt in, as one JPEG: see a clip before planning. Details: klipp_guide topic contact_sheet.",
             json!({
                 "media_id": { "type": "string" },
                 "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                 "frames": { "type": "integer" }, "cols": { "type": "integer" },
                 "tile_width": { "type": "integer" },
                 "path": { "type": "string", "description": "Absolute JPEG path; temp dir when omitted" },
                 "inline": { "type": "boolean", "description": "Also return the picture in the reply; by default only when no path is given" },
             }),
             vec!["media_id"]),
        tool("klipp_audio_excerpt",
             "Hear a media file (media_id) or the timeline's mix between two times, as WAV. \
              Details: klipp_guide topic audio_excerpt.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                     "sequence": { "type": "boolean" },
                     "path": { "type": "string" } }),
             vec!["from_secs", "to_secs"]),
        tool("klipp_plan",
             "Dry run: what a list of editorial operations would do, step by step, with each touched clip's range afterwards. Nothing changes. Returns a revision for a guarded klipp_apply. Ops: klipp_guide topic ops.",
             json!({ "ops": { "type": "array", "items": { "type": "object" } },
                     "detail": { "type": "boolean", "description": "every step in full and each touched clip's range as JSON; off by default, when long runs of one kind of step are folded" } }),
             vec!["ops"]),
        tool("klipp_apply",
             "Apply editorial operations as ONE undo step, resolved as klipp_plan does; expected_revision refuses a changed timeline. Ops: klipp_guide topic ops; details: topic apply.",
             json!({ "ops": { "type": "array", "items": { "type": "object" } },
                     "expected_revision": { "type": "integer", "minimum": 0, "description": "Revision from klipp_plan or a timeline read; refuse if it changed" },
                     "expected_project_identity": { "type": "integer", "minimum": 0, "description": "Optional project identity from klipp_plan; refuse if the open project changed" },
                     "label": { "type": "string", "description": "A name for the history entry" },
                     "detail": { "type": "boolean", "description": "every step in full and each touched clip's range as JSON; off by default, when the reply is the plan as text with long runs of one kind of step folded" } }),
             vec!["ops"]),
        tool("klipp_undo", "One step back in the active sequence's edit history. Every write \
              made through these tools is one step; a klipp_batch is one step as a whole.",
             json!({}), vec![]),
        tool("klipp_redo", "One step forward in the edit history.", json!({}), vec![]),
        tool("klipp_batch",
             "Several write tools applied together as ONE undo step; every call is checked first and a bad one refuses the whole batch. Reads are refused inside. Details: klipp_guide topic batch.",
             json!({ "calls": { "type": "array", "description": "In order. Each: {\"tool\": name, \"args\": {...}}",
                 "items": { "type": "object", "properties": {
                     "tool": { "type": "string" }, "args": { "type": "object" } }, "required": ["tool"] } } }),
             vec!["calls"]),
        tool("klipp_export",
             "Render the active sequence to an absolute path with the defaults, a preset, or a codec, bit depth and bitrate; asynchronous. Details: klipp_guide topic export.",
             json!({
                 "path": { "type": "string", "description": "Absolute output path" },
                 "width": { "type": "integer" }, "height": { "type": "integer" },
                 "fps": { "type": "number" },
                 "crf": { "type": "integer", "description": "Quality, lower is better (default 23)" },
                 "target_lufs": { "type": "number", "description": "Loudness target: -14 web, -23 broadcast; default the brief's" },
                 "true_peak_dbtp": { "type": "number", "description": "True-peak ceiling, -10 to 0 (default -1; ATSC A/85 -2)" },
                 "burn_captions": { "type": "boolean", "description": "Captions and titles burnt in (default on); the reply says which" },
                 "captions_608": { "type": "boolean", "description": "Captions as CEA-608 inside the video stream (h264/hevc)" },
                 "hdr10plus": { "type": "boolean", "description": "Per-shot ST 2094-40 metadata (hevc, PQ)" },
                 "preset": export_preset_schema(),
                 "encoder_speed": { "type": "string", "enum": klipp_control::ENCODER_SPEEDS },
                 "codec": { "type": "string" }, "audio_codec": { "type": "string" },
                 "bit_depth": { "type": "integer" }, "bitrate_kbps": { "type": "integer" },
             }),
             vec!["path"]),
        tool("klipp_export_status",
             "Every render job: status, progress, error, loudness and where its time went; wait true waits for one or all. Details: klipp_guide topic export_status.",
             json!({ "wait": { "type": "boolean" }, "job_id": { "type": "string" }, "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour" } }), vec![]),
        tool("klipp_export_subtitles",
             "Write the subtitle clips (text clips if none) to an absolute path, as the extension says: .srt, .vtt, .ass, .ssa, .ttml (IMSC), .scc (608) or .stl (EBU); format, if given, must match.",
             json!({ "path": { "type": "string" }, "format": { "type": "string" } }), vec!["path"]),
        tool("klipp_translate_captions",
             "Translate a subtitle track onto a new one under it. Details: klipp_guide topic translate_captions.",
             json!({ "to": { "type": "string", "description": "de fr es sv it nl fi da, or en" }, "from": { "type": "string", "description": "default en" }, "track": { "type": "integer" }, "download_models": { "type": "boolean" } }), vec!["to"]),
        tool("klipp_import_subtitles",
             "Read an .srt, .vtt, .ass, .ssa or TTML (.ttml, .dfxp, .xml, .itt), .scc or .stl file (absolute path) onto a new subtitle track.",
             json!({ "path": { "type": "string" } }), vec!["path"]),
        tool("klipp_transcript",
             "What is said in a media file between two times, by segment, and with words true word by word, with the measured silences. Details: klipp_guide topic transcript.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "words": { "type": "boolean" } }),
             vec!["media_id"]),
        tool("klipp_cut_points",
             "The clean places to cut in a media file, ranked: pauses, then sentence ends and shot changes, then phrase breaks; near_secs judges one moment. Details: klipp_guide topic cut_points.",
             json!({ "media_id": { "type": "string" }, "near_secs": { "type": "number" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" } }),
             vec!["media_id"]),
        tool("klipp_prepare_cut",
             "Ranked frame-exact cut candidates in a timeline range, from pauses, words, shots and screen reveals. Reads only. Details: klipp_guide topic prepare_cut.",
             json!({ "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                     "near_secs": { "type": "number" }, "intent": { "type": "string" },
                     "max_candidates": { "type": "integer", "minimum": 1, "maximum": 32 } }),
             vec!["from_secs", "to_secs"]),
        tool("klipp_selects",
             "A long take's selects: the runs of speech between pauses with their text, each marked keep or drop. Reads only. Details: klipp_guide topic selects.",
             json!({ "media_id": { "type": "string" }, "min_gap_secs": { "type": "number", "description": "the pause length that splits one select from the next, seconds" } }),
             vec!["media_id"]),
        tool("klipp_beats",
             "A clip's tempo and beat grid, and for music its sections and energy; with target_secs, place ops that fit it to a length. Reads only. Details: klipp_guide topic beats.",
             json!({ "media_id": { "type": "string" }, "target_secs": { "type": "number", "description": "fit the track to this length, seconds" }, "skip_intro": { "type": "boolean", "description": "with target_secs: leave the intro out and fit from where it ends" }, "at_secs": { "type": "number", "description": "where the fitted track's place ops start on the timeline (0)" }, "track": { "description": "the track for the place ops: an index, or a kind such as \"audio\" (the first audio track)" } }), vec!["media_id"]),
        tool("klipp_style_profile",
             "A reference video's editing style as facts (cut cadence, tempo, look, motion, holds), to cut other footage in its feel. Reads only. Details: klipp_guide topic style_profile.",
             json!({ "media_id": { "type": "string" } }), vec!["media_id"]),
        tool("klipp_search_music",
             "Search royalty-free music on Jamendo by mood or genre, with licences; listen previews up to five. Import with klipp_add_music. Details: klipp_guide topic search_music.",
             json!({ "query": { "type": "string" }, "limit": { "type": "integer" }, "commercial_only": { "type": "boolean", "description": "only tracks whose licence allows commercial use" }, "instrumental": { "type": "boolean", "description": "only instrumental tracks, for a promo with its own titles" }, "edits_only": { "type": "boolean", "description": "no ND" }, "licences": { "type": "array", "items": { "type": "string", "enum": klipp_control::LICENCE_FAMILIES } }, "listen": { "description": "hear previews (at most 5): the first this-many, or [track_id, …]; tempo, beat and energy, nothing imported", "oneOf": [{ "type": "integer" }, { "type": "array", "items": { "type": "string" } }] } }), vec!["query"]),
        tool("klipp_search_sfx",
             "Search royalty-free sound effects on Freesound by keyword, with licences; listen hears up to five and says where each peaks. Import with klipp_add_music. Details: klipp_guide topic search_sfx.",
             json!({ "query": { "type": "string" }, "limit": { "type": "integer" }, "max_secs": { "type": "number", "description": "only sounds this long or shorter" }, "commercial_only": { "type": "boolean", "description": "only sounds whose licence allows commercial use" }, "listen": { "description": "hear previews (at most 5): the first this-many, or [track_id, …]; length, peak time, loudness", "oneOf": [{ "type": "integer" }, { "type": "array", "items": { "type": "string" } }] } }), vec!["query"]),
        tool("klipp_add_music",
             "Download a searched music track or sound effect into the pool, its licence and attribution kept for the credits. Details: klipp_guide topic add_music.",
             json!({ "title": { "type": "string" }, "artist": { "type": "string" }, "track_id": { "type": "string" }, "download_url": { "type": "string" }, "preview_url": { "type": "string" }, "license": { "type": "string" }, "license_url": { "type": "string" }, "page_url": { "type": "string" }, "tracks": { "type": "array", "items": { "type": "object" } }, "quality": { "type": "string", "enum": ["original", "preview"] } }),
             vec![]),
        tool("klipp_freesound_sign_in",
             "Freesound sign-in, for original sounds. Details: klipp_guide topic freesound_sign_in.",
             json!({ "code": { "type": "string" }, "sign_out": { "type": "boolean" } }), vec![]),
        tool("klipp_match_grade",
             "Match picture clips to a reference by scope measurements (exposure, lift, gain, temperature, tint), as one undo step. Details: klipp_guide topic match_grade.",
             json!({ "clip_id": { "type": "string", "description": "the shot to grade" }, "reference_clip_id": { "type": "string", "description": "the shot to match it to" }, "secs": { "type": "number", "description": "the target frame to read, timeline seconds inside the clip" }, "reference_secs": { "type": "number", "description": "the reference frame to read, timeline seconds inside it" } }),
             vec!["clip_id", "reference_clip_id"]),
        tool("klipp_align_audio",
             "Sync a clip's sound to another's by cross-correlating their audio, and move it there; reports the confidence. Details: klipp_guide topic align_audio.",
             json!({ "clip_id": { "type": "string", "description": "the clip to move into sync (e.g. the external audio)" }, "reference_clip_id": { "type": "string", "description": "the clip to sync to (e.g. the camera)" }, "max_offset_secs": { "type": "number" } }),
             vec!["clip_id", "reference_clip_id"]),
        tool("klipp_title_templates",
             "Title templates, built-in by category and saved, and their fields by label, for the template op.",
             json!({}), vec![]),
        tool("klipp_fonts",
             "The font families a title can name. Details: klipp_guide topic fonts.",
             json!({}), vec![]),
        tool("klipp_graph",
             "Atomic graph edit, one undo step; read graph_read/node_types first. Guide: klipp_guide topic graph.",
             json!({"ops":graph_schema::ops_schema(),"label":{"type":"string"},"expected_revision":{"type":"integer","minimum":0},"expected_project_identity":{"type":"integer","minimum":0}}), vec!["ops"]),
        tool("klipp_graph_read",
             "Read graph at path; empty nodes reads root. Depth controls nested bodies. Omissions are flagged; \
              narrow path for details. mask_channel_set marks an explicitly stored channel.",
             json!({"path":graph_schema::path_schema(true),"depth":{"type":"integer","minimum":0,"maximum":4,"default":1}}), vec!["path"]),
        tool("klipp_node_types",
             "Node ports and parameters, optionally by category. Guide: klipp_guide topic node_types.",
             json!({ "category": { "type": "string", "description": "a category to list, e.g. Colour" } }), vec![]),
        tool("klipp_looks",
             "The bundled looks (3D LUTs) by category: the names klipp_look takes. Details: klipp_guide topic looks.",
             json!({ "category": { "type": "string", "description": "a category to list in full, e.g. cinematic or film-print" } }), vec![]),
        tool("klipp_look",
             "Apply a bundled 3D LUT after the grade on a clip, the timeline or a grade group, as one undo step; remove clears it. Details: klipp_guide topic look.",
             json!({ "clip_id": { "type": "string" }, "on": { "description": "instead of clip_id: \"timeline\", {\"group\": name, \"stage\": \"post\" (default) or \"pre\"}, or {\"track\": index or kind, \"from\"?, \"to\"?} for every picture clip of a track, with \"itself\": true for the track's own grade" }, "name": { "type": "string", "description": "a look name from klipp_looks, or an absolute .cube path" }, "opacity": { "type": "number" }, "remove": { "type": "boolean" } }),
             vec![]),
        tool("klipp_orient",
             "Start here: the project, the brief, the media and what to do next, in one call. Details: klipp_guide topic orient.",
             json!({}), vec![]),
        tool("klipp_brief",
             "The brief: what the piece is for. Read it before you plan. Details: klipp_guide topic brief.",
             json!({}), vec![]),
        tool("klipp_set_brief",
             "Replace the project brief as one undo step. With merge true, change only supplied fields; null clears a field. Details: klipp_guide topic set_brief.",
             json!({ "brief": { "type": "object" }, "merge": { "type": "boolean", "description": "change only the fields given; the rest stay" } }), vec!["brief"]),
        tool("klipp_edit_plan",
             "The edit plan as text and JSON, whether it is approved, and its ops. Details: klipp_guide topic edit_plan.",
             json!({}), vec![]),
        tool("klipp_set_edit_plan",
             "Propose a paper edit (segments, music, montage, titles) for user approval as one undo step. Show the resulting plan to the user and ask. Details: klipp_guide topic set_edit_plan.",
             json!({ "plan": { "type": "object" } }), vec!["plan"]),
        tool("klipp_approve_edit_plan",
             "Mark the edit plan approved, one undo step, only once the user said to build it. Details: klipp_guide topic approve_edit_plan.",
             json!({}), vec![]),
        tool("klipp_build_edit_plan",
             "Build the approved edit plan onto the timeline: a dry run unless apply is true, then one undo step. Details: klipp_guide topic build_edit_plan.",
             json!({ "apply": { "type": "boolean" }, "detail": { "type": "boolean" } }), vec![]),
        tool("klipp_check",
             "Read-only checks of the cut before export, against craft and the brief: each finding with where, why and the fix. Optional colour and loudness passes. Details: klipp_guide topic check.",
             json!({ "color": { "type": "boolean" }, "loudness": { "type": "boolean" } }), vec![]),
        tool("klipp_review_finding",
             "Decide a klipp_check finding by id: accepted (with rationale), to_fix or clear. Details: klipp_guide topic review_finding.",
             json!({ "finding_id": { "type": "string" }, "decision": { "type": "string" }, "rationale": { "type": "string" } }), vec!["finding_id", "decision"]),
        tool("klipp_rename_sequence",
             "Name the active sequence. Details: klipp_guide topic rename_sequence.",
             json!({ "name": { "type": "string" } }), vec!["name"]),
        tool("klipp_switch_sequence",
             "Open another of the project's sequences, as clicking its tab does: by index or sequence_id, as \
              klipp_status lists them. The tools then work on it.",
             json!({ "index": { "type": "integer" }, "sequence_id": { "type": "string" } }), vec![]),
        tool("klipp_create_multicam",
             "Make a multicam group from two or more pool angles of one event, synced by timecode or sound and laid out at the playhead, in the background. Details: klipp_guide topic create_multicam.",
             json!({
                 "media_ids": { "type": "array", "items": { "type": "string" }, "description": "the angles, from klipp_list_media" },
                 "sync": { "type": "string", "enum": ["timecode", "audio", "timecode_then_audio"] }
             }), vec!["media_ids"]),
        tool("klipp_switch_angle",
             "Cut the multicam group in play to an angle, counted from 1, at at_secs or the playhead, as one undo step. Details: klipp_guide topic switch_angle.",
             json!({
                 "angle": { "type": "integer", "minimum": 1 },
                 "at_secs": { "type": "number", "description": "timeline seconds; the playhead when left out" }
             }), vec!["angle"]),
        tool("klipp_set_multicam_sound",
             "Choose whose sound the multicam group plays: an angle, follow the picture, or off. Details: klipp_guide topic set_multicam_sound.",
             json!({
                 "sound": { "description": "an angle number from 1, \"follow\" or \"off\"" },
                 "at_secs": { "type": "number", "description": "timeline seconds; the playhead when left out" }
             }), vec!["sound"]),
        tool("klipp_add_marker",
             "Put a marker, a point or a range, on the timeline with a label, note, colour and kind; answers with its id. Details: klipp_guide topic add_marker.",
             json!({
                 "at_secs": { "type": "number" },
                 "duration_secs": { "type": "number" },
                 "label": { "type": "string" },
                 "note": { "type": "string", "description": "the longer comment" },
                 "color": { "type": "string", "enum": ["red", "orange", "yellow", "green", "blue", "purple"] },
                 "kind": { "type": "string", "enum": ["chapter", "todo", "comment", "best_take", "section"] }
             }), vec!["at_secs"]),
        tool("klipp_list_markers",
             "Every timeline marker in time order: id, time, label, note, kind; a todo is work asked for. Details: klipp_guide topic list_markers.",
             json!({}), vec![]),
        tool("klipp_update_marker",
             "Change a marker's label, note, colour or kind, by id (from klipp_list_markers). \
              Mark a todo done by changing its kind or colour, rather than removing the note.",
             json!({
                 "marker_id": { "type": "string" },
                 "label": { "type": "string" },
                 "note": { "type": "string" },
                 "color": { "type": "string", "enum": ["red", "orange", "yellow", "green", "blue", "purple"] },
                 "kind": { "type": "string", "enum": ["none", "chapter", "todo", "comment", "best_take", "section"] }
             }), vec!["marker_id"]),
        tool("klipp_remove_marker",
             "Remove a marker, by id.",
             json!({ "marker_id": { "type": "string" } }), vec!["marker_id"]),
        tool("klipp_new_project",
             "Start an empty project at <location>/<name>.klipp, saved at once; refused over unsaved changes unless discard_changes is true. Details: klipp_guide topic new_project.",
             json!({
                 "name": { "type": "string" },
                 "location": { "type": "string", "description": "absolute path of an existing folder" },
                 "width": { "type": "integer" },
                 "height": { "type": "integer" },
                 "fps": { "type": "number", "description": "25, 29.97, 30, 59.94 …" },
                 "sequence_name": { "type": "string" },
                 "discard_changes": { "type": "boolean" },
                 "template": { "type": "string", "description": "a project template's name (klipp_project_templates)" }
             }), vec!["name", "location"]),
        tool("klipp_project_templates",
             "The user's project templates: what each holds and needs. With save, keep the open project as one first. Details: klipp_guide topic project_templates.",
             json!({
                 "save": { "type": "object", "properties": {
                     "name": { "type": "string" }, "description": { "type": "string" },
                     "starter_sequences": { "type": "array", "items": { "type": "string" } },
                     "package_media": { "type": "boolean" }
                 }, "required": ["name"] }
             }), vec![]),
        tool("klipp_new_sequence",
             "Add a sequence at a picture size and rate, and open it. Details: klipp_guide topic new_sequence.",
             json!({
                 "name": { "type": "string" },
                 "width": { "type": "integer" },
                 "height": { "type": "integer" },
                 "fps": { "type": "number" }
             }), vec!["name", "width", "height", "fps"]),
        tool("klipp_save_project",
             "Save the project to an absolute .klipp folder, or where it was last saved. Details: klipp_guide topic save_project.",
             json!({ "path": { "type": "string" } }), vec![]),
        tool("klipp_relink_media",
             "Point a media item at a file, judged against what the project knew of it; a probable mismatch is refused unless force is true. Details: klipp_guide topic relink_media.",
             json!({
                 "media_id": { "type": "string" },
                 "path": { "type": "string", "description": "absolute path of the file" },
                 "force": { "type": "boolean", "description": "relink even a file the check says is probably not the clip" }
             }), vec!["media_id", "path"]),
        tool("klipp_tag_media",
             "Tag a media item with words that sort the pool (a person, a place, \"b-roll\", \
              \"hero\"), or remove tags. klipp_list_media shows each item's tags.",
             json!({
                 "media_id": { "type": "string" },
                 "add": { "type": "array", "items": { "type": "string" } },
                 "remove": { "type": "array", "items": { "type": "string" } }
             }), vec!["media_id"]),
        tool("klipp_window_screenshot",
             "Window PNG for UI checks, not colour measurement. Guide: klipp_guide topic window_screenshot.",
             json!({ "path": { "type": "string" }, "max_width": { "type": "integer" } }), vec![]),
        tool("klipp_import_timeline",
             "Import or open a timeline; no path: last outcome. Guide: klipp_guide topic import_timeline.",
             json!({"path":{"type":"string"},"new_project":{"type":"boolean"}}), vec![]),
        tool("klipp_open_project",
             "Open a project by path or recent name; protect unsaved changes. Guide: klipp_guide topic open_project.",
             json!({ "path": { "type": "string" }, "name": { "type": "string" }, "save": { "type": "boolean" }, "discard_changes": { "type": "boolean" } }), vec![]),
        tool("klipp_recent_projects", "List existing recent projects, most recent first. Names and paths are local project data. Guide: klipp_guide topic recent_projects.", json!({}), vec![]),
        tool("klipp_close_project", "Close to the project page; refuses unsaved changes unless save or discard_changes. Guide: klipp_guide topic close_project.", json!({"save":{"type":"boolean"},"discard_changes":{"type":"boolean"}}), vec![]),
        tool("klipp_frame_stats",
             "Frame scope measurements against the brief. Guide: klipp_guide topic frame_stats.",
             json!({ "secs": { "type": "number" }, "timeout_ms": { "type": "integer" } }), vec!["secs"]),
        tool("klipp_timeline_sheet",
             "Timeline contact sheet by shot or evenly spaced frames. Guide: klipp_guide topic timeline_sheet.",
             json!({
                 "from_secs": { "type": "number" },
                 "to_secs": { "type": "number" },
                 "frames": { "type": "integer", "description": "Evenly spaced tiles instead of one per shot (1-60)" },
                 "cols": { "type": "integer", "description": "Tiles to a row (default 6)" },
                 "tile_width": { "type": "integer", "description": "Pixels (default 320)" },
                 "path": { "type": "string", "description": "Absolute PNG path; temp dir when omitted" },
                 "inline": { "type": "boolean", "description": "Also return the picture in the reply; by default only when no path is given" },
             }), vec![]),
        tool("klipp_frame_at",
             "The timeline's frame at a time, as a PNG, the playhead left where it was. Details: klipp_guide topic frame_at.",
             json!({
                 "secs": { "type": "number" },
                 "max_width": { "type": "integer", "description": "Downscale to at most this wide (default 1280)" },
                 "path": { "type": "string", "description": "Absolute PNG path; temp dir when omitted" },
                 "timeout_ms": { "type": "integer" },
             }),
             vec!["secs"]),
        tool("klipp_scope",
             "A GPU-drawn waveform, parade, vectorscope or histogram of a frame as a PNG, without disturbing the app's own scopes. Details: klipp_guide topic scope.",
             json!({
                 "mode": { "type": "string", "description": "waveform, parade, vectorscope, or histogram (waveform by default)" },
                 "secs": { "type": "number", "description": "timeline seconds; the playhead when omitted" },
                 "path": { "type": "string", "description": "Absolute PNG path; temp dir when omitted" },
                 "timeout_ms": { "type": "integer" },
             }),
             vec![]),
    ];
    if structured {
        for tool in &mut tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                if has_structured_output(name) { tool["outputSchema"] = output_schema(name); }
            }
        }
    }
    tools
}

#[cfg(test)]
mod sheet_inline_tests {
    use super::*;

    /// #796: a sheet written where it was asked comes back as its path
    /// and tiles, not a megabyte of picture.
    #[test]
    fn a_sheet_with_a_path_is_not_inlined() {
        let dir = std::env::temp_dir().join(format!("klipp-mcp-sheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("sheet.png");
        std::fs::write(&png, [0x89u8, b'P', b'N', b'G', 0, 0, 0, 0]).unwrap();
        let reply = |inline: bool| json!({ "text": "sheet", "path": png.to_string_lossy(), "tiles": [], "inline": inline });
        let kept = content_for("klipp_timeline_sheet", &reply(false)).unwrap();
        assert!(kept.iter().all(|c| c["type"] == "text"), "{kept:?}");
        let shown = content_for("klipp_timeline_sheet", &reply(true)).unwrap();
        assert!(shown.iter().any(|c| c["type"] == "image"), "{shown:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod listen_arg_tests {
    use super::*;

    /// #891: a grade parameter keyed at a time goes through with its
    /// time, interpolation and removal; a plain set still needs a value.
    #[test]
    fn set_node_param_keys_at_a_time() {
        let base = json!({ "clip_id": "c", "node_id": "n", "param": "exposure" });
        let mut keyed = base.clone();
        keyed["value"] = json!(1.0);
        keyed["at"] = json!(2.5);
        keyed["interpolation"] = json!("ease_out");
        let req = tool_to_command("klipp_set_node_param", &keyed).unwrap();
        assert_eq!((req["method"].clone(), req["at"].clone(), req["interpolation"].clone(), req["value"].clone(), req["remove"].clone()), (json!("set_node_param"), json!(2.5), json!("ease_out"), json!(1.0), json!(false)));
        let mut removed = base.clone();
        removed["at"] = json!(2.5);
        removed["remove"] = json!(true);
        let req = tool_to_command("klipp_set_node_param", &removed).unwrap();
        assert_eq!((req["remove"].clone(), req["value"].clone()), (json!(true), Value::Null));
        assert!(tool_to_command("klipp_set_node_param", &base).unwrap_err().contains("value"));
    }

    /// #795: listen goes through as a count or as a list of ids.
    #[test]
    fn listen_is_a_count_or_ids() {
        assert_eq!(listen_arg(&json!({ "listen": 3 })), Some(json!(3)));
        assert_eq!(listen_arg(&json!({ "listen": ["12", 34] })), Some(json!(["12", "34"])));
        assert_eq!(listen_arg(&json!({})), None);
    }
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn timeline_import_routes_reads_and_writes_and_has_a_guide() {
        assert_eq!(tool_to_command("klipp_import_timeline", &json!({"path":"/tmp/test.otio"})).unwrap(), json!({"method":"import_timeline","path":"/tmp/test.otio","new_project":false}));
        assert_eq!(tool_to_command("klipp_import_timeline", &json!({"path":"/tmp/test.otio","new_project":true})).unwrap(), json!({"method":"import_timeline","path":"/tmp/test.otio","new_project":true}));
        assert_eq!(tool_to_command("klipp_import_timeline", &json!({})).unwrap(), json!({"method":"import_timeline","path":null,"new_project":false}));
        assert!(guide_topic("import_timeline").unwrap().contains("warning counts") || guide_topic("import_timeline").unwrap().contains("Warning counts"));
        assert!(has_structured_output("klipp_import_timeline"));
        let reply = json!({"report":{"imported":{"clips":1}},"request_id":"request"});
        let content = content_for("klipp_import_timeline", &reply).unwrap();
        assert_eq!(serde_json::from_str::<Value>(content[0]["text"].as_str().unwrap()).unwrap(), reply);
        let size = serde_json::to_vec(&tool_definitions_for(false)).unwrap().len();
        assert!(size < 60_000, "catalogue is {size} bytes");
    }

    #[test]
    fn graph_calls_preserve_paths_references_and_guards() {
        let path = json!({"root":{"clip":"00000000-0000-0000-0000-000000000001"},"nodes":[{"new":0}]});
        let args = json!({"ops":[{"op":"enable","path":path,"enabled":false}],"expected_revision":17,"expected_project_identity":4,"idempotency_key":"same-write"});
        let mut expected = args.clone();
        expected["method"] = json!("graph_ops");
        assert_eq!(tool_to_command("klipp_graph", &args).unwrap(), expected);
        assert_eq!(tool_to_command("klipp_graph_read", &json!({"path":path,"depth":2})).unwrap(), json!({"method":"graph_read","path":path,"depth":2}));
        let definitions = tool_definitions_for(false);
        let graph = definitions.iter().find(|t| t["name"] == "klipp_graph").unwrap();
        let ops = graph["inputSchema"]["properties"]["ops"]["items"]["properties"]["op"]["enum"].as_array().unwrap();
        assert_eq!(ops.len(), 12);
        for op in [json!({"op":"publish","path":path,"from":path,"from_port":"out","key":"matte"}),
                   json!({"op":"unpublish","path":path,"key":"matte"})] {
            assert!(ops.contains(&op["op"]));
            let args = json!({"ops":[op]});
            assert_eq!(tool_to_command("klipp_graph", &args).unwrap()["ops"], args["ops"]);
        }
        let beside = json!({"ops":[{"op":"add_beside","at":path,"type":"klipp.grade.mask"}]});
        let command = tool_to_command("klipp_graph", &beside).unwrap();
        assert_eq!(command["ops"], beside["ops"]);
        assert!(ops.iter().any(|op| op == "add_beside"));
    }

    #[test]
    fn compact_graph_guidance_still_has_strict_wire_validation() {
        let path = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[uuid::Uuid::nil()]});
        for op in [
            json!({"op":"enable","path":path,"enabled":true,"value":1}),
            json!({"op":"enable","path":path}),
            json!({"op":"mix","path":path}),
            json!({"op":"set","path":{"root":{"clip":uuid::Uuid::nil(),"track":uuid::Uuid::nil()},"nodes":[]},"key":"x","value":1}),
            json!({"op":"publish","path":path}),
        ] {
            let error = tool_to_command("klipp_graph", &json!({"ops":[op]})).unwrap_err();
            let error: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(error["op_index"],0);
            assert_eq!(error["applied"],false);
        }
        let tools = tool_definitions_for(false);
        for (name, budget) in [("klipp_graph",2900),("klipp_graph_read",700)] {
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            let size = serde_json::to_vec(tool).unwrap().len();
            assert!(size <= budget,"{name}: {size} > {budget}");
        }
        let catalog = serde_json::to_vec(&tools).unwrap().len();
        // Trimmed to 58 KB (#1984): the catalog goes into every agent's
        // context, so it is held with room to spare, not let creep up.
        assert!(catalog < 60_000, "keep the catalogue short; details go in guide topics (#1667, #1984): {catalog} bytes");
        // Every write tool takes a retry key, its use said once, in
        // klipp_guide, not again on each tool (#1984).
        let guide = tools.iter().find(|t| t["name"] == "klipp_guide").unwrap();
        assert!(guide["description"].as_str().unwrap().contains("idempotency_key"));
        for t in tools.iter().filter(|t| WRITE_TOOLS.contains(&t["name"].as_str().unwrap())) {
            let key = &t["inputSchema"]["properties"]["idempotency_key"];
            assert_eq!(key, &json!({ "type": "string" }), "{}", t["name"]);
        }
    }

    /// #1667: a tool's description names the guide topic its details are
    /// in, and every topic named is there; every tool page's section is a
    /// tool's, and the op topic lists every op.
    #[test]
    fn every_guide_topic_a_tool_names_is_there() {
        let tools = tool_definitions_for(false);
        let catalogue = serde_json::to_string(&tools).unwrap();
        let named: Vec<&str> = catalogue.match_indices("klipp_guide topic ").map(|(at, m)| {
            let rest = &catalogue[at + m.len()..];
            &rest[..rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len())]
        }).collect();
        assert!(named.len() > 60, "{} topics named", named.len());
        for topic in &named {
            let text = guide_topic(topic).unwrap_or_else(|e| panic!("{topic}: {e}"));
            assert!(text.len() > 40, "{topic}");
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for (page, text) in TOOL_PAGES {
            assert_eq!(guide_topic(page).unwrap(), *text);
            for line in text.lines().filter_map(|l| l.strip_prefix("## ")) {
                assert!(names.contains(&line), "{page} has a section for {line}, which is not a tool");
                let one = guide_topic(&line["klipp_".len()..]).unwrap();
                assert!(one.starts_with(&format!("## {line}\n")) && !one[3..].contains("\n## "), "{line}: {one}");
                assert_eq!(guide_topic(line).unwrap(), one, "the full name reads the same");
            }
        }
        let ops = guide_topic("ops").unwrap();
        for op in klipp_document::ops::OP_NAMES {
            assert!(ops.contains(op), "{op}");
        }
        assert!(ops.starts_with("## Ops\n") && ops.contains("`place {media_id, track, at"), "the guide's op list first");
        assert!(ops.contains("# More on the ops") && ops.contains("bridge_secs"), "then the fuller notes");
        assert!(!ops.contains("## Batches"), "and the list ends where the guide's next part starts");
        let err = guide_topic("nope").unwrap_err();
        assert!(err.contains("ops") && err.contains("timeline-tools") && err.contains("a tool's name"), "{err}");
    }

    fn describe_nested_for_test() -> String {
        let props = json!({
            "clips": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "secs": { "type": "number" } }, "required": ["id"] } },
            "options": { "type": "object" },
            "names": { "type": "array", "items": { "type": "string" } },
        });
        let mut out = String::new();
        describe_arguments(props.as_object().unwrap(), &json!([]), 0, &mut out);
        out
    }

    /// #1898: every tool in the catalog answers as a guide topic, by its
    /// name with or without `klipp_`: its own section where it has one,
    /// else its catalog entry, its arguments with it.
    #[test]
    fn every_tool_is_a_guide_topic() {
        let tools = tool_definitions_for(false);
        assert!(tools.len() > 100, "{} tools", tools.len());
        for tool in &tools {
            let name = tool["name"].as_str().unwrap();
            let text = guide_topic(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(text.starts_with(&format!("## {name}\n")), "{name}: {text}");
            // The short name reads the same, but for a tool named as a
            // page is (klipp_guide, klipp_graph): that name is the page.
            let short = &name["klipp_".len()..];
            if !["guide", "graph", "craft", "recipes", "abilities", "ops", "all"].contains(&short) {
                assert_eq!(guide_topic(short).unwrap(), text, "{name}: the short name reads the same");
            }
            let generated = !TOOL_PAGES.iter().any(|(_, page)| page.contains(&format!("## {name}\n")));
            for arg in tool["inputSchema"]["properties"].as_object().into_iter().flat_map(|p| p.keys()) {
                if generated {
                    assert!(text.contains(&format!("`{arg}`")), "{name} names its argument {arg}: {text}");
                }
            }
            if generated {
                assert!(text.contains("No guide section yet"), "{name}: a made entry says it is one: {text}");
            }
        }
        // An object or array argument shows what is inside it, or says it
        // isn't listed (#1898 review).
        let nested = describe_nested_for_test();
        assert!(nested.contains("each item's fields:") && nested.contains("    - `id` (string, required)"), "{nested}");
        assert!(nested.contains("(its fields are not listed here"), "{nested}");
        // The report's tool has its own section now, and a made-up one is
        // still unknown.
        let excerpt = guide_topic("audio_excerpt").unwrap();
        assert!(excerpt.contains("sixty seconds") && excerpt.contains("sequence"), "{excerpt}");
        assert!(guide_topic("klipp_teleport").is_err());
    }

    /// #1899: the export's preset lists the delivery presets the app
    /// takes, and its encoder speed is a field apart.
    #[test]
    fn the_export_lists_its_presets_and_speeds() {
        let tools = tool_definitions_for(false);
        let export = tools.iter().find(|t| t["name"] == "klipp_export").unwrap();
        let props = &export["inputSchema"]["properties"];
        // Every preset the export takes is listed where the schema says.
        let guide = guide_topic("export").unwrap();
        for (name, ext) in klipp_control::export_presets() {
            assert!(guide.contains(&format!("{name} (.{ext})")), "{name} in the export guide: {guide}");
        }
        assert!(props["preset"]["description"].as_str().unwrap().contains("klipp_guide topic export"));
        assert!(props.get("encoder_speed").is_some(), "the speed is a field apart");
        let speeds: Vec<&str> = props["encoder_speed"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(speeds, klipp_control::ENCODER_SPEEDS);
        let v = tool_to_command("klipp_export", &json!({"path": "/tmp/a.mp4", "encoder_speed": "slow"})).unwrap();
        assert_eq!(v["encoder_speed"], "slow");
    }

    #[test]
    fn misspelled_graph_guards_are_refused_before_a_socket_call() {
        let path = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[]});
        for key in ["expected_revison", "expected_project_identty", "lable", "method", "id"] {
            let mut args = json!({"ops":[{"op":"enable","path":path,"enabled":false}]});
            args[key] = json!(1);
            assert!(tool_to_command("klipp_graph", &args).unwrap_err().contains(key));
        }
        assert!(tool_to_command("klipp_graph_read", &json!({"path":path,"depht":2})).unwrap_err().contains("depht"));
    }

    /// Claude Code cuts an MCP tool's description off at about 2,048
    /// characters, without a word, and sends the agent only that much
    /// (measured for #688: the token cost stops rising there). Before #669
    /// `klipp_apply` and `klipp_plan` carried the op reference in 17,000
    /// characters of description each, and the agent saw about the first
    /// eighth. What does not fit belongs in the input schema, which is not
    /// cut, or in `klipp_guide`.
    #[test]
    fn every_tool_description_fits_the_clients_cut_off() {
        const CUT_OFF: usize = 2048;
        for structured in [false, true] {
            let long: Vec<String> = tool_definitions_for(structured)
                .iter()
                .filter_map(|t| {
                    let chars = t["description"].as_str().unwrap_or("").chars().count();
                    (chars > CUT_OFF).then(|| format!("{} ({chars} characters)", t["name"].as_str().unwrap_or("?")))
                })
                .collect();
            assert!(long.is_empty(), "past the {CUT_OFF}-character cut-off, so the agent would not see the end (structured {structured}): {long:?}");
        }
    }

    #[test]
    fn negotiated_catalog_is_compact_and_old_clients_keep_text() {
        assert_eq!(negotiated_version(Some("2024-11-05")), LEGACY_PROTOCOL_VERSION);
        assert_eq!(negotiated_version(Some("2025-06-18")), STRUCTURED_PROTOCOL_VERSION);
        assert_eq!(negotiated_version(Some("unknown")), LEGACY_PROTOCOL_VERSION);
        let legacy = tool_definitions_for(false);
        let modern = tool_definitions_for(true);
        assert_eq!(legacy.len(), modern.len());
        let catalog_bytes = serde_json::to_vec(&legacy).unwrap().len();
        eprintln!("legacy tool catalog: {} tools, {catalog_bytes} bytes", legacy.len());
        assert!(catalog_bytes < 90_000, "catalog is {catalog_bytes} bytes");
        let plan = modern.iter().find(|tool| tool["name"] == "klipp_plan").unwrap();
        assert_eq!(plan["inputSchema"]["properties"]["ops"]["items"]["oneOf"][0]["properties"]["op"]["const"], "cut");
        assert_eq!(plan["inputSchema"]["properties"]["ops"]["items"]["oneOf"][0]["properties"]["from"]["$ref"], "#/properties/ops/$defs/anchor");
        assert_eq!(plan["outputSchema"]["type"], "object");
        assert!(legacy.iter().all(|tool| tool.get("outputSchema").is_none()));
    }

    #[test]
    fn malformed_edit_is_refused_before_any_socket_call() {
        let error = tool_to_command("klipp_apply", &json!({"ops":[{"op":"cut","from":{"frame":1},"to":{"fram":2}}]})).unwrap_err();
        assert!(error.contains("anchor form"), "{error}");
    }

    #[test]
    fn track_aliases_match_the_document_resolver() {
        for kind in ["Title", "text", "CAPTIONS", "subtitles", "Video", "AUDIO"] {
            assert!(edit_schema::validate_ops(&json!({"ops":[{"op":"add_track","kind":kind}]})).is_ok(), "{kind}");
            assert!(edit_schema::validate_ops(&json!({"ops":[{"op":"place","media_id":"m","track":kind,"at":{"frame":0}}]})).is_ok(), "{kind}");
        }
    }
}

#[cfg(test)]
mod friendly_answer_tests {
    use super::*;

    #[test]
    fn grade_to_target_keeps_the_current_revision_in_text() {
        for applied in [false, true] {
            let reply = json!({ "text": "Two shots checked.", "revision": 87, "applied": applied, "undo_steps": 1, "next": "check with color" });
            let content = content_for("klipp_grade_to_target", &reply).unwrap();
            let text = content[0]["text"].as_str().unwrap();
            assert!(text.contains("Revision: 87"), "the next edit needs the current revision: {text}");
            assert!(text.contains("Next: check with color"));
        }
    }

    #[test]
    fn grade_to_target_declares_structured_revision_output() {
        let tools = tool_definitions_for(true);
        let grade = tools.iter().find(|tool| tool["name"] == "klipp_grade_to_target").unwrap();
        assert_eq!(grade["outputSchema"]["properties"]["revision"]["type"], "integer");
        assert!(has_structured_output("klipp_grade_to_target"), "modern clients must receive the raw revision too");
    }

    /// #626: a status reply leads with the plain summary a person is told,
    /// the fields after it; the guide answers "what can I do"; and the
    /// server tells the client how to answer both questions.
    #[test]
    fn the_first_answers_are_in_a_persons_words() {
        let reply = json!({ "text": "Klipp 0.1.0 is running (build abc).\nProject: \u{201c}Trip\u{201d}, all changes saved.", "revision": 3, "clips": 0 });
        let content = content_for("klipp_status", &reply).unwrap();
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.starts_with("Klipp 0.1.0 is running"), "{text}");
        assert!(text.contains("Details:") && text.contains("\"revision\": 3") && !text.contains("\"text\""), "{text}");

        let abilities = call_tool("klipp_guide", &json!({ "topic": "abilities" })).unwrap();
        let abilities = abilities.0[0]["text"].as_str().unwrap();
        assert!(abilities.starts_with("# What you can do in Klipp"), "{abilities}");
        assert!(!abilities.contains("klipp_apply"), "no tool names a person would have to read");

        for asked in ["klipp_status", "klipp_guide with topic abilities", "nothing was changed", "No tool names"] {
            assert!(INSTRUCTIONS.contains(asked), "the instructions say {asked:?}");
        }
    }
}

#[cfg(test)]
mod wait_tests {
    use super::*;

    #[test]
    fn queued_apply_tells_the_agent_to_check_the_actual_outcome() {
        let command = tool_to_command("klipp_apply", &json!({
            "ops": [{"op":"cut","from":{"frame":1},"to":{"frame":2}}], "idempotency_key": "edit-42",
        })).unwrap();
        assert_eq!(command["idempotency_key"], "edit-42");
        assert_eq!(tool_to_command("klipp_operation_status", &json!({ "operation_id": "write-42" })).unwrap()["operation_id"], "write-42");
        let pending = content_for("klipp_apply", &json!({
            "text": "one split\n", "applied": false, "operation_id": "write-42", "next": "look at the cut",
        })).unwrap();
        let text = pending[0]["text"].as_str().unwrap();
        assert!(text.contains("klipp_operation_status") && text.contains("write-42"), "{text}");
        assert!(!text.contains("Applied as one undo step"), "{text}");
        assert!(!text.contains("look at the cut"), "a queued write must not advance the agent: {text}");
    }

    /// #618: project templates are listed, saved, and started from.
    #[test]
    fn project_templates_reach_the_socket() {
        assert_eq!(tool_to_command("klipp_project_templates", &json!({})).unwrap(), json!({ "method": "project_templates" }));
        let save = json!({ "name": "Weekly", "starter_sequences": ["Episode"], "package_media": true });
        assert_eq!(tool_to_command("klipp_project_templates", &json!({ "save": save.clone() })).unwrap(), json!({ "method": "project_templates", "save": save }));
        let made = tool_to_command("klipp_new_project", &json!({ "name": "Ep 12", "location": "/tmp", "template": "Weekly" })).unwrap();
        assert_eq!((made["method"].as_str(), made["template"].as_str()), (Some("new_project"), Some("Weekly")));
        let request: klipp_control::protocol::Request = serde_json::from_value(json!({ "id": 1, "method": "project_templates", "save": { "name": "Weekly", "starter_sequences": ["Episode"] } })).unwrap_or_else(|e| panic!("{e}"));
        let _ = request;
    }

    #[test]
    fn retry_keys_are_advertised_for_background_writes_and_belong_on_the_outer_batch() {
        let tools = tool_definitions();
        for name in ["klipp_apply", "klipp_import_media", "klipp_analyze", "klipp_add_marker", "klipp_new_project", "klipp_batch"] {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert!(tool["inputSchema"]["properties"]["idempotency_key"].is_object(), "{name}");
        }
        let inner_key = tool_to_command("klipp_batch", &json!({ "calls": [
            { "tool": "klipp_seek", "args": { "secs": 2.0, "idempotency_key": "inside" } }
        ] })).unwrap_err();
        assert!(inner_key.contains("put idempotency_key on the batch"), "{inner_key}");
        let batch = tool_to_command("klipp_batch", &json!({ "idempotency_key": "outside", "calls": [
            { "tool": "klipp_seek", "args": { "secs": 2.0 } }
        ] })).unwrap();
        assert_eq!(batch["idempotency_key"], "outside");
        assert!(batch["commands"][0].get("idempotency_key").is_none());
    }

    #[test]
    fn cut_preparation_keeps_timeline_range_and_candidate_limit() {
        let command = tool_to_command("klipp_prepare_cut", &json!({
            "from_secs": 10.0, "to_secs": 15.0, "near_secs": 12.0,
            "intent": "remove the pause after the sentence", "max_candidates": 8,
        })).unwrap();
        assert_eq!(command["method"], "prepare_cut");
        assert_eq!(command["from_secs"], 10.0);
        assert_eq!(command["near_secs"], 12.0);
        assert_eq!(command["max_candidates"], 8);
    }

    #[test]
    fn apply_carries_plan_preconditions_and_plan_exposes_them() {
        let ops = json!([{ "op": "cut", "from": { "frame": 2 }, "to": { "frame": 3 } }]);
        let command = tool_to_command("klipp_apply", &json!({
            "ops": ops, "expected_revision": 8, "expected_project_identity": 3,
        })).unwrap();
        assert_eq!(command["expected_revision"], 8);
        assert_eq!(command["expected_project_identity"], 3);
        assert!(tool_to_command("klipp_apply", &json!({ "ops": ops, "expected_revision": "8" })).is_err());

        let plan = content_for("klipp_plan", &json!({ "text": "one cut", "revision": 8, "project_identity": 3 })).unwrap();
        let text = plan[0]["text"].as_str().unwrap();
        assert!(text.contains("expected_revision: 8") && text.contains("expected_project_identity: 3"));
        let incomplete = content_for("klipp_plan", &json!({ "text": "one cut", "revision": 8 })).unwrap();
        assert_eq!(incomplete[0]["text"], "one cut");
    }

    #[test]
    fn project_access_tools_forward_selection_and_safety_options() {
        let open=tool_to_command("klipp_open_project",&json!({"name":"Film","save":true})).unwrap();
        assert_eq!(open["name"],"Film"); assert_eq!(open["save"],true); assert!(open["path"].is_null());
        let close=tool_to_command("klipp_close_project",&json!({"discard_changes":true})).unwrap();
        assert_eq!(close,json!({"method":"close_project","save":false,"discard_changes":true}));
        assert_eq!(tool_to_command("klipp_recent_projects",&json!({})).unwrap(),json!({"method":"recent_projects"}));
        assert!(!project_changed_under("klipp_close_project",1,2));
        assert!(WRITE_TOOLS.contains(&"klipp_close_project"));
        for bad in [json!({"save":"true"}),json!({"save":true,"discard_changes":true})] {
            assert!(tool_to_command("klipp_close_project",&bad).is_err());
        }
        assert!(tool_to_command("klipp_open_project",&json!({"path":"/tmp/film.klipp","name":"Film"})).is_err());
        assert!(tool_to_command("klipp_open_project",&json!({"path":17,"name":"Film"})).is_err());
        for name in ["klipp_recent_projects","klipp_close_project","klipp_open_project"] {
            assert!(tool_definitions().iter().any(|t|t["name"]==name));
            assert!(guide_topic(&name[6..]).is_ok());
        }
    }

    #[test]
    fn every_advertised_tool_has_a_dispatch_path() {
        let tools = tool_definitions();
        let mut names = std::collections::HashSet::new();
        for definition in tools {
            let name = definition["name"].as_str().expect("tool has a name").to_owned();
            assert!(names.insert(name.clone()), "tool is advertised more than once: {name}");
            if name == "klipp_guide" {
                continue;
            }

            // Empty arguments deliberately exercise only routing. A required
            // argument is expected to produce a validation error; an
            // `unknown tool` error means the advertised name has no dispatch
            // arm in tool_to_command.
            if let Err(error) = tool_to_command(&name, &json!({})) {
                assert!(!error.starts_with("unknown tool:"), "advertised tool is not dispatchable: {name}");
            }
        }
    }

    #[test]
    fn set_brief_with_merge_sends_only_the_fields() {
        let whole = tool_to_command("klipp_set_brief", &json!({ "brief": { "notes": "n" } })).unwrap();
        assert_eq!(whole, json!({ "method": "set_brief", "brief": { "notes": "n" } }));
        let merged = tool_to_command("klipp_set_brief", &json!({ "brief": { "notes": "n" }, "merge": true })).unwrap();
        assert_eq!(merged, json!({ "method": "update_brief", "fields": { "notes": "n" } }));
        assert!(tool_to_command("klipp_set_brief", &json!({ "brief": { "notes": "n" }, "merge": "true" })).is_err(), "a string is not a yes");
    }

    #[test]
    fn import_analysis_defaults_on_and_queue_options_are_forwarded() {
        let old = tool_to_command("klipp_import_media", &json!({ "path": "/tmp/take.mp4" })).unwrap();
        assert_eq!(old["analyze"], true);
        let skip = tool_to_command("klipp_import_media", &json!({ "path": "/tmp/take.mp4", "analyze": false })).unwrap();
        assert_eq!(skip["analyze"], false);
        // Many at once (#1587): paths and a folder go through, and
        // something to import is required.
        let many = tool_to_command("klipp_import_media", &json!({ "paths": ["/tmp/a.mp4", "/tmp/b.mp4"], "folder": "/tmp/shots" })).unwrap();
        assert_eq!(many["paths"], json!(["/tmp/a.mp4", "/tmp/b.mp4"]));
        assert_eq!(many["folder"], "/tmp/shots");
        assert!(many.get("path").is_none());
        assert!(tool_to_command("klipp_import_media", &json!({ "analyze": false })).is_err());
        let music = tool_to_command("klipp_add_music", &json!({ "tracks": [{ "title": "a" }, { "title": "b" }] })).unwrap();
        assert_eq!(music["tracks"][1]["title"], "b");
        let queue = tool_to_command("klipp_analysis_queue", &json!({ "media_id": "take", "cancel_pending": true })).unwrap();
        assert_eq!(queue["media_id"], "take");
        assert_eq!(queue["cancel_pending"], true);
    }

    /// #239: Track Subject reaches the socket as a request it parses.
    #[test]
    fn track_subject_reaches_the_socket() {
        let call = tool_to_command("klipp_track_subject", &json!({ "clip_id": "c1", "model": "foreground", "download_model": true })).unwrap();
        assert_eq!(call, json!({ "method": "track_subject", "clip_id": "c1", "model": "foreground", "download_model": true }));
        assert_eq!(tool_to_command("klipp_track_subject", &json!({ "clip_id": "c1" })).unwrap(), json!({ "method": "track_subject", "clip_id": "c1" }));
        assert!(tool_to_command("klipp_track_subject", &json!({})).is_err(), "a clip is needed");
        let request: klipp_control::protocol::Request = serde_json::from_value(json!({ "id": 1, "method": "track_subject", "clip_id": "c1" })).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(request.command, klipp_control::protocol::ControlCommand::TrackSubject { model: None, download_model: false, .. }));
        let cut = tool_to_command("klipp_track_subject", &json!({ "clip_id": "c1", "into": "cut_out" })).unwrap();
        assert_eq!(cut["into"], "cut_out");
        let request: klipp_control::protocol::Request = serde_json::from_value(json!({ "id": 2, "method": "track_subject", "clip_id": "c1", "into": "cut_out" })).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(request.command, klipp_control::protocol::ControlCommand::TrackSubject { into: Some(ref i), .. } if i == "cut_out"));
        let whole = tool_to_command("klipp_track_subject", &json!({ "clip_id": "c1", "into": "whole" })).unwrap();
        assert_eq!(whole["into"], "whole");
        let transition = tool_to_command("klipp_track_subject", &json!({ "transition_id": "t1" })).unwrap();
        assert_eq!(transition, json!({ "method": "track_subject", "transition_id": "t1" }));
    }

    #[test]
    fn wait_arguments_reach_control_and_old_calls_stay_nonblocking() {
        for (tool, method, id_key) in [
            ("klipp_analyze", "analyze", "media_id"),
            ("klipp_analysis_status", "analysis_status", "media_id"),
            ("klipp_export_status", "export_status", "job_id"),
        ] {
            let args = json!({ "wait": true, "timeout_ms": 123, id_key: "id" });
            let command = tool_to_command(tool, &args).unwrap();
            assert_eq!(command["method"], method);
            assert_eq!(command["wait"], true);
            assert_eq!(command["timeout_ms"], 123);
            assert_eq!(command[id_key], "id");
            assert_eq!(tool_to_command(tool, &json!({})).unwrap()["wait"], false);
        }
    }
}

#[cfg(test)]
mod generator_tests {
    use super::*;

    #[test]
    fn generators_preview_by_default_and_forward_native_parameters() {
        let data = json!({"Gradient": {"top": [16, 32, 48, 255], "bottom": [64, 80, 96, 255], "angle_deg": 45}});
        let mut args = json!({"data": data, "at_secs": 1.0, "secs": 2.0});
        let preview = tool_to_command("klipp_generator", &args).unwrap();
        assert_eq!(preview["method"], "plan");
        assert_eq!(preview["ops"][0]["data"], data);
        assert_eq!(preview["ops"][0]["at"]["secs"], 1.0);
        assert_eq!(preview["ops"][0]["secs"], 2.0);
        assert!(preview["ops"][0]["track"].is_null());
        args["apply"] = json!(true);
        args["track"] = json!(2);
        let apply = tool_to_command("klipp_generator", &args).unwrap();
        assert_eq!(apply["method"], "apply");
        assert_eq!(apply["ops"][0]["track"], 2);
        args["data"] = json!("Countdown");
        assert_eq!(tool_to_command("klipp_generator", &args).unwrap()["ops"][0]["data"], "Countdown");
        assert!(tool_to_command("klipp_generator", &json!({})).is_err());
        let text = content_for("klipp_generator", &json!({"text": "place generator", "next": "check"})).unwrap();
        assert!(text[0]["text"].as_str().unwrap().contains("place generator"));
    }
}

#[cfg(test)]
mod clips_text_tests {
    use super::*;

    /// #507: list_clips reads as JSON with a clip to a line, and is still
    /// the same JSON.
    #[test]
    fn a_clip_is_a_line_and_the_whole_is_json() {
        let v = json!({ "fps": 25.0, "revision": 3, "tracks": [
            { "index": 0, "kind": "Video", "name": "V1", "clips": [
                { "clip_id": "a", "kind": "media", "in_secs": 0.0, "out_secs": 2.0 },
                { "clip_id": "b", "kind": "title", "in_secs": 2.0, "out_secs": 3.0, "name": "Hello" },
            ]},
            { "index": 1, "kind": "Audio", "name": "A1", "clips": [] },
        ]});
        let text = clips_text(&v);
        let back: Value = serde_json::from_str(&text).expect(&text);
        assert_eq!(back, v);
        assert_eq!(text.lines().filter(|l| l.contains("clip_id")).count(), 2, "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("{\"clip_id\":\"b\"")), "{text}");
    }
}

#[cfg(test)]
mod project_change_notice_tests {
    use super::*;

    /// #1574: the call that made the new project is not told the
    /// project changed under it; the next unrelated call after an
    /// outside change still is.
    #[test]
    fn the_call_that_changes_the_project_is_not_warned() {
        assert!(!project_changed_under("klipp_new_project", 1, 2));
        assert!(!project_changed_under("klipp_open_project", 1, 2));
        assert!(!project_changed_under("klipp_save_project", 1, 2));
        assert!(project_changed_under("klipp_timeline", 1, 2));
        assert!(!project_changed_under("klipp_timeline", 2, 2));
        assert!(!project_changed_under("klipp_timeline", u64::MAX, 2));
    }
}

    /// klipp_switch_sequence asks the socket's switch_sequence by index or
    /// id, and is refused with neither or both before a socket call.
    #[test]
    fn switch_sequence_takes_an_index_or_an_id() {
        assert_eq!(tool_to_command("klipp_switch_sequence", &json!({ "index": 1 })).unwrap(), json!({ "method": "switch_sequence", "index": 1 }));
        assert_eq!(tool_to_command("klipp_switch_sequence", &json!({ "sequence_id": "abc" })).unwrap(), json!({ "method": "switch_sequence", "sequence_id": "abc" }));
        for args in [json!({}), json!({ "index": 0, "sequence_id": "abc" })] {
            assert!(tool_to_command("klipp_switch_sequence", &args).unwrap_err().contains("index or sequence_id"), "{args}");
        }
    }

    /// #1516: subtitles are read over MCP too, and written in a format asked for.
    #[test]
    fn subtitles_are_imported_and_exported_in_a_format() {
        assert_eq!(tool_to_command("klipp_import_subtitles", &json!({ "path": "/a/b.srt" })).unwrap(), json!({ "method": "import_subtitles", "path": "/a/b.srt" }));
        assert_eq!(tool_to_command("klipp_translate_captions", &json!({ "to": "de" })).unwrap(), json!({ "method": "translate_captions", "to": "de", "download_models": false }));
        assert_eq!(
            tool_to_command("klipp_translate_captions", &json!({ "to": "en", "from": "sv", "track": 3, "download_models": true })).unwrap(),
            json!({ "method": "translate_captions", "to": "en", "from": "sv", "track": 3, "download_models": true })
        );
        assert!(tool_to_command("klipp_translate_captions", &json!({})).is_err(), "to is needed");
        assert_eq!(tool_to_command("klipp_export_subtitles", &json!({ "path": "/a/b.vtt", "format": "vtt" })).unwrap(), json!({ "method": "export_subtitles", "path": "/a/b.vtt", "format": "vtt" }));
        assert_eq!(tool_to_command("klipp_export_subtitles", &json!({ "path": "/a/b.srt" })).unwrap(), json!({ "method": "export_subtitles", "path": "/a/b.srt" }));
    }

#[cfg(test)]
mod review_boundary_tests {
    use super::*;

    #[test]
    fn source_sync_survives_plan_and_apply_validation() {
        for name in ["klipp_plan", "klipp_apply"] {
            let args = json!({"ops":[{"op":"place","media_id":"sound","track":"audio","at":{"secs":2},"source_in":0.1,"source_out":1,"sync_source_secs":0.35}]});
            let command = tool_to_command(name, &args).unwrap();
            assert_eq!(command["ops"][0]["sync_source_secs"], 0.35);
            for bad in [json!(-1), json!("0.35"), Value::Null] {
                let mut invalid = args.clone();
                invalid["ops"][0]["sync_source_secs"] = bad;
                assert!(tool_to_command(name, &invalid).is_err());
            }
        }
        let schema = edit_schema::ops_schema();
        let place = schema["items"]["oneOf"].as_array().unwrap().iter().find(|v| v["properties"]["op"]["const"] == "place").unwrap();
        assert_eq!(place["properties"]["sync_source_secs"]["minimum"], 0);
    }

    #[test]
    fn check_and_review_expose_actionable_ids_to_both_clients() {
        let check = json!({"text":"one warning", "findings":[{"id":"check-title-1","what":"short title"}], "color_findings":[{"id":"color-1","what":"clipping"}], "sound_findings":[{"id":"sound-1","what":"hum"}]});
        for name in ["klipp_check", "klipp_review"] {
            assert!(has_structured_output(name));
            let result = if name == "klipp_review" { json!({"text":"review", "check":check}) } else { check.clone() };
            let content = content_for(name, &result).unwrap();
            let text = content[0]["text"].as_str().unwrap();
            for id in ["check-title-1", "color-1", "sound-1"] {
                assert!(text.contains(id), "{name}: {text}");
                let review = tool_to_command("klipp_review_finding", &json!({"finding_id":id,"decision":"accepted","rationale":"intentional"})).unwrap();
                assert_eq!(review["finding_id"], id);
            }
        }
    }
}
