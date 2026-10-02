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

mod edit_schema;
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
    "klipp_play", "klipp_pause", "klipp_stop", "klipp_seek",
    "klipp_append_clip", "klipp_remove_clip", "klipp_attach_audio", "klipp_set_append_audio", "klipp_select_clip",
    "klipp_add_node", "klipp_remove_node", "klipp_connect_nodes",
    "klipp_set_node_param", "klipp_toggle_node", "klipp_set_log_format",
    "klipp_generator", "klipp_undo", "klipp_redo", "klipp_batch", "klipp_export", "klipp_export_subtitles", "klipp_apply",
    "klipp_open_project",
];

/// Calls with side effects may be replayed after a lost socket reply. The
/// control server owns deduplication, including tools that queue background
/// analysis or export work rather than changing the viewer immediately.
const RETRYABLE_TOOLS: &[&str] = &[
    "klipp_analyze", "klipp_analysis_queue", "klipp_import_media", "klipp_grade_to_target",
    "klipp_adopt_look", "klipp_match_scenes", "klipp_grade_chain", "klipp_review",
    "klipp_narrate", "klipp_record", "klipp_auto_sfx", "klipp_auto_redact",
    "klipp_add_music", "klipp_match_grade", "klipp_align_audio", "klipp_look",
    "klipp_orient", "klipp_set_brief", "klipp_set_edit_plan", "klipp_approve_edit_plan",
    "klipp_build_edit_plan", "klipp_rename_sequence", "klipp_create_multicam",
    "klipp_switch_angle", "klipp_set_multicam_sound", "klipp_add_marker",
    "klipp_update_marker", "klipp_remove_marker", "klipp_new_project", "klipp_new_sequence",
    "klipp_save_project", "klipp_relink_media", "klipp_tag_media",
    "klipp_cancel_job",
];

fn main() {
    // `klipp-mcp --check`: is Klipp there for an assistant? The same
    // read-only status an assistant asks for first, from the same binary
    // it runs, said in words; exit 0 when it answered (#625). What
    // Preferences → AI runs to check a connection, and a person can run
    // it in a terminal.
    if std::env::args().nth(1).as_deref() == Some("--check") {
        match control_call(json!({ "method": "status" })) {
            Ok(v) => {
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
        println!("klipp-mcp {}", env!("CARGO_PKG_VERSION"));
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
                    "serverInfo": { "name": "klipp", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }
            })},
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions_for(structured.load(Ordering::Acquire)) },
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
        | "search_music" | "search_sfx" | "add_music" | "grade_to_target" | "match_scenes")
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

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

/// The socket command a tool call is. Shared by a single call and by the
/// calls inside a batch.
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
        "klipp_import_media" => json!({
            "method": "import_media", "path": arg_str(args, "path")?, "analyze": args.get("analyze").and_then(Value::as_bool).unwrap_or(true),
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
            let mut v = json!({ "method": "analyze", "wait": args.get("wait").and_then(Value::as_bool).unwrap_or(false), "timeout_ms": args.get("timeout_ms") });
            if let Some(id) = args.get("media_id").and_then(|v| v.as_str()) {
                v["media_id"] = json!(id);
            }
            if let Some(d) = args.get("download_models").and_then(|v| v.as_bool()) {
                v["download_models"] = json!(d);
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
        "klipp_auto_redact" => json!({ "method": "auto_redact", "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_skin_balance" => json!({ "method": "skin_balance", "clip_id": arg_str(args, "clip_id")?, "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_grade_to_target" => json!({
            "method": "grade_to_target",
            "clip_id": args.get("clip_id").cloned().unwrap_or(serde_json::Value::Null),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "rounds": args.get("rounds").cloned().unwrap_or(serde_json::Value::Null),
            "force": args.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
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
            "download_models": args.get("download_models").and_then(|v| v.as_bool()).unwrap_or(false),
            "apply": args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "path": args.get("path").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_record" => json!({
            "method": "record",
            "secs": args.get("secs").and_then(|v| v.as_f64()).ok_or("missing required argument: secs")?,
            "target": args.get("target").cloned().unwrap_or(serde_json::Value::Null),
            "fps": args.get("fps").cloned().unwrap_or(serde_json::Value::Null),
            "path": args.get("path").cloned().unwrap_or(serde_json::Value::Null),
            "import": args.get("import").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "klipp_auto_sfx" => json!({
            "method": "auto_sfx",
            "whoosh": args.get("whoosh").cloned().unwrap_or(serde_json::Value::Null),
            "click": args.get("click").cloned().unwrap_or(serde_json::Value::Null),
            "riser": args.get("riser").cloned().unwrap_or(serde_json::Value::Null),
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
        "klipp_beats" => json!({ "method": "beats", "media_id": arg_str(args, "media_id")? }),
        "klipp_search_music" => {
            let mut v = json!({ "method": "search_music", "query": arg_str(args, "query")? });
            if let Some(x) = args.get("limit").and_then(|v| v.as_u64()) { v["limit"] = json!(x); }
            if let Some(c) = args.get("commercial_only").and_then(|v| v.as_bool()) { v["commercial_only"] = json!(c); }
            if let Some(c) = args.get("instrumental").and_then(|v| v.as_bool()) { v["instrumental"] = json!(c); }
            v
        }
        "klipp_search_sfx" => {
            let mut v = json!({ "method": "search_sfx", "query": arg_str(args, "query")? });
            if let Some(x) = args.get("limit").and_then(|v| v.as_u64()) { v["limit"] = json!(x); }
            v
        }
        "klipp_add_music" => {
            let mut v = json!({ "method": "add_music" });
            for key in ["title", "artist", "track_id", "download_url", "preview_url", "license", "license_url", "page_url"] {
                if let Some(x) = args.get(key).and_then(|v| v.as_str()) { v[key] = json!(x); }
            }
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
        "klipp_export_subtitles" => json!({ "method": "export_subtitles", "path": arg_str(args, "path")? }),
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
        "klipp_check" => json!({ "method": "check", "color": args.get("color").and_then(|v| v.as_bool()).unwrap_or(false) }),
        "klipp_rename_sequence" => json!({ "method": "rename_sequence", "name": arg_str(args, "name")? }),
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
        "klipp_new_project" => {
            let mut v = json!({ "method": "new_project", "name": arg_str(args, "name")?, "location": arg_str(args, "location")? });
            for key in ["width", "height", "fps", "sequence_name", "discard_changes"] {
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
        "klipp_open_project" => json!({
            "method": "open_project",
            "path": arg_str(args, "path")?,
            "discard_changes": args.get("discard_changes").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        "klipp_frame_stats" => json!({ "method": "frame_stats", "secs": args.get("secs").and_then(|v| v.as_f64()).ok_or("missing required argument: secs")?, "timeout_ms": args.get("timeout_ms").and_then(|v| v.as_u64()) }),
        "klipp_scope" => {
            let mut v = json!({ "method": "scope", "mode": args.get("mode").and_then(|v| v.as_str()).unwrap_or("waveform") });
            if let Some(x) = args.get("secs").and_then(|v| v.as_f64()) { v["secs"] = json!(x); }
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) { v["path"] = json!(p); }
            if let Some(t) = args.get("timeout_ms").and_then(|v| v.as_u64()) { v["timeout_ms"] = json!(t); }
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
const CRAFT: &str = include_str!("../../../assets/craft/reference.md");
const RECIPES: &str = include_str!("../../../assets/craft/recipes.md");
/// What a person can do here, for answering them when they ask (#626).
const ABILITIES: &str = include_str!("../../../assets/craft/abilities.md");

/// Sent with `initialize`, which clients put before the model in every
/// session: how to answer the two things a person asks first (#626).
const INSTRUCTIONS: &str = "Klipp is a video editor running on this computer; these tools work in the open project.

When the person asks whether Klipp is connected, open or working: call klipp_status (it reads and changes nothing) and answer from its text, in plain words: yes or no first, then the version, the open project and whether it is saved, the timeline, and the playhead, then say that nothing was changed. No socket paths, ids or JSON. If the call fails, say Klipp does not seem to be running and to start it and ask again.

When the person asks what they can do with Klipp, what you can do in it, or how to start: read klipp_guide with topic abilities and answer from it, short and in their words: a sentence on what working together looks like, the groups that fit with an example request each, then one first step that fits what is open (read with klipp_status), and last, that there is more, naming two or three other areas they can ask about. No tool names.

Before editing, read klipp_guide (topic guide) for how to work here.";

fn call_tool(name: &str, args: &Value) -> Result<(Vec<Value>, Option<Value>), String> {
    if name == "klipp_guide" {
        let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("guide");
        let text = match topic {
            "guide" => GUIDE.to_string(),
            "craft" => CRAFT.to_string(),
            "recipes" => RECIPES.to_string(),
            "abilities" => ABILITIES.to_string(),
            "all" => format!("{GUIDE}\n\n---\n\n{CRAFT}\n\n---\n\n{RECIPES}"),
            other => return Err(format!("unknown topic {other}; use guide, craft, recipes, abilities or all")),
        };
        return Ok((vec![json!({ "type": "text", "text": text })], None));
    }
    let mut cmd = tool_to_command(name, args)?;
    if (WRITE_TOOLS.contains(&name) || RETRYABLE_TOOLS.contains(&name))
        && cmd.get("idempotency_key").is_none()
    {
        cmd["idempotency_key"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let result = control_call(cmd)?;
    // The project changed under this session — made new, opened or saved
    // as — since its last call: said, so what it knew about the timeline
    // and the pool is not trusted a moment longer.
    // By the project's identity, which a save where it already was does
    // not move; the generation moves on every save, so a Ctrl+S in the
    // app used to raise this for nothing.
    let mut notice: Option<String> = None;
    if let Some(g) = result.get("project_identity").or_else(|| result.get("project_generation")).and_then(|v| v.as_u64()) {
        let seen = LAST_PROJECT_GENERATION.swap(g, std::sync::atomic::Ordering::Relaxed);
        if seen != u64::MAX && seen != g && !matches!(name, "klipp_open_project" | "klipp_save_project") {
            notice = Some("Note: the project changed under you since your last call (a new project, or one opened or saved as another) — the timeline, the media pool and their ids are not what they were. Read klipp_status, klipp_list_media and klipp_timeline again before going on.".into());
        }
    }

    let mut content = content_for(name, &result)?;
    if let Some(n) = notice {
        content.insert(0, json!({ "type": "text", "text": n }));
    }
    Ok((content, Some(result)))
}

fn has_structured_output(name: &str) -> bool {
    matches!(name, "klipp_status" | "klipp_timeline" | "klipp_plan" | "klipp_apply" | "klipp_prepare_cut")
}

fn output_schema(name: &str) -> Value {
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
            return Ok(with_next(text.to_string()));
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
            return Ok(with_next(message));
        }
    }
    if name == "klipp_list_clips" {
        return Ok(vec![json!({ "type": "text", "text": clips_text(&result) })]);
    }
    // Pictures and sound come back as a path; inline them too when
    // small enough to be worth a context window.
    let inline = match name {
        "klipp_screenshot" | "klipp_frame_at" | "klipp_scope" => Some(("image", "image/png")),
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

fn tool(name: &str, description: &str, mut properties: Value, required: Vec<&str>) -> Value {
    if WRITE_TOOLS.contains(&name) || RETRYABLE_TOOLS.contains(&name) {
        properties["idempotency_key"] = json!({
            "type": "string",
            "description": "Reuse the same key when retrying this exact write after a lost reply; Klipp keeps keys only while this app run lasts"
        });
    }
    // Keep every tool's guidance discoverable. For the few large argument
    // references, put the complete reference on the relevant input field.
    let description = match name {
        "klipp_generator" => {
            properties["data"]["description"] = json!(description);
            "Preview a native generator clip; apply true places it as one undo step. Give at_secs and secs; data describes the native generator. Overlaps and locked tracks are refused."
        }
        "klipp_set_brief" => {
            properties["brief"]["description"] = json!(description);
            "Replace the project brief as one undo step. With merge true, change only supplied fields; null clears a field. Read the brief field for the full format."
        }
        "klipp_set_edit_plan" => {
            properties["plan"]["description"] = json!(description);
            "Propose a paper edit for user approval as one undo step. Read the plan field for segment, music, montage and title formats. Show the resulting plan to the user and ask."
        }
        "klipp_narrate" => {
            properties["voice"]["description"] = json!(description);
            "Create local narration and import the take; apply true places it on a Voice track. Nothing downloads unless download_models is true. Read the voice field for voices, licences and prerequisites."
        }
        _ => description,
    };
    let description = match name {
        "klipp_guide" => "Read the editable workflow, full operation reference and recipes. Read before editing.",
        "klipp_plan" => "Preview frame-exact editorial ops without changing the timeline. Returns a revision for a guarded klipp_apply. Read klipp_guide for the full operation reference.",
        "klipp_apply" => "Apply frame-exact editorial ops as one undo step. Pass expected_revision from a recent plan or timeline read. Read klipp_guide for the full operation reference.",
        _ => description,
    };
    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if matches!(name, "klipp_plan" | "klipp_apply") {
        properties["ops"] = edit_schema::ops_schema();
    }
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

#[cfg(test)]
fn tool_definitions() -> Vec<Value> { tool_definitions_for(false) }

fn tool_definitions_for(structured: bool) -> Vec<Value> {
    let mut tools = vec![
        tool("klipp_guide",
             "Read this first. How to work in Klipp as an agent (the loop: read, learn the \
              footage, plan, apply, look, deliver; units, ids, anchors, ops, manners), the \
              craft reference (what a professional does without being asked), and the \
              recipes (worked plans for an interview with B-roll, a social cut-down, a \
              montage). Files the user can read and change.",
             json!({ "topic": { "type": "string", "description": "guide (default), craft, recipes, abilities (what a person can do here, for answering them when they ask), or all" } }),
             vec![]),
        tool("klipp_status",
             "Whether Klipp is there and what is open, read live without changing anything: \
              first a plain summary to tell a person who asks whether Klipp is connected \
              (version, project and whether it is saved, timeline, playhead, media), then \
              the details: the build (commit, dirty, when it was built, and whether a newer \
              build is on disk so a restart is due), transport state, timeline summary, \
              viewer resolution and the latest performance sample. Start here.",
             json!({}), vec![]),
        tool("klipp_operation_status",
             "Read the latest known outcome of a queued write by its operation_id. Reports waiting_for_ui, ui_applied or ui_refused, the final document revision and refusal reason. Outcomes are kept only while Klipp runs; an ID from before a restart says so. Progress is null when the UI has no measurable total; this tracks UI command completion, not any background render or import probe.",
             json!({ "operation_id": { "type": "string" } }), vec!["operation_id"]),
        tool("klipp_job_status",
             "Read live render and analysis jobs in one shape, including phase, progress, result and error. Give job_id to follow one job. With wait true, return on a progress change or after a bounded long poll; call again until completed, failed or cancelled.",
             json!({ "job_id": { "type": "string" }, "wait": { "type": "boolean" },
                     "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 10000 } }), vec![]),
        tool("klipp_follow_job",
             "Wait for one render or analysis job to end, then say how it ended: completed with the file and its \
              loudness, failed with why, or cancelled, and the way there (each 10% with the time). Waits up to \
              max_wait_ms (10 minutes unless given, at most an hour); if the job is still running then, says so — \
              call again. Your client may show progress meanwhile, but you see only this reply. For live progress \
              you can tell the person as it happens, run `klipp-mcp follow <job_id>` as a background task \
              instead: it prints a line per step and a last line when the job ends.",
             json!({ "job_id": { "type": "string" },
                     "max_wait_ms": { "type": "integer", "minimum": 1, "maximum": 3600000 } }), vec!["job_id"]),
        tool("klipp_cancel_job",
             "Cancel the named render or one queued analysis job. Running analysis, import and sync cannot be interrupted safely. A queued UI cancellation has its own operation_id; confirm its outcome before saying the job stopped.",
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
             "Every track and its clips, one line a clip: clip id, kind, range in \
              seconds, name (file, title text or generator) and media id, with speed, \
              off, graded and effects only where set. Narrow it to a track, a stretch \
              of time or one media item; full adds source ranges, frames, blend, grade \
              group and paths. Each track lists its transitions too: kind, length, \
              the stretch they cover and their two clips. Clip ids from here are what \
              the grade tools take.",
             json!({
                 "track": { "type": ["integer", "string"], "description": "A track's index or name" },
                 "from_secs": { "type": "number", "description": "Only clips that end after this" },
                 "to_secs": { "type": "number", "description": "Only clips that start before this" },
                 "media_id": { "type": "string", "description": "Only clips of this media item" },
                 "full": { "type": "boolean", "description": "Every field (default false)" },
             }),
             vec![]),
        tool("klipp_list_media",
             "The media pool: media ids, paths, duration, fps, resolution, codec.",
             json!({}), vec![]),
        tool("klipp_generator",
             "Preview a native generator clip; apply true places it as one undo step. Give at_secs, secs (positive, at most one day), optional video track index, and data in the native generator format: {SolidColor:{color:[r,g,b,a]}}, {Gradient:{top:[r,g,b,a],bottom:[r,g,b,a],angle_deg:0}}, {LivingGradient:{a:[r,g,b,a],b:[r,g,b,a],angle_deg:20,drift_deg_per_sec:4,glow:0.35}}, {Grain:{color:[r,g,b,a],amount:0.35,size_px:2,speed:24}}, {Polygon:{sides:24,color:[r,g,b,a],fill:true}} for a circle, {Placeholder:{label:missing,color:[r,g,b,a]}}, or the string Countdown. Colour channels are bytes 0..255. Omit track to use/create a bottom Background video track. Overlaps and locked tracks are refused.",
             json!({ "data": { "type": ["object", "string"] }, "at_secs": { "type": "number" }, "secs": { "type": "number" }, "track": { "type": "integer" }, "apply": { "type": "boolean" } }), vec!["data", "at_secs", "secs"]),
        tool("klipp_import_media",
             "Import a file into the media pool. Asynchronous — the app probes the \
              file on a worker thread, so call klipp_list_media afterwards to see it.",
             json!({ "path": { "type": "string", "description": "Absolute path to a media file" }, "analyze": { "type": "boolean", "description": "Automatically analyze on import (default true); false leaves the queue untouched" } }),
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
        tool("klipp_detect_scene_cuts",
             "Split a clip at the cuts in its picture: a flattened movie, a montage someone exported, \
              a long take with edits in it, back into its shots on the timeline. Runs in the \
              background; the clip is then split at each hard cut, its linked sound with it, as one \
              undo step. Hard cuts are found well; slow dissolves may be missed. Read klipp_timeline \
              after a while for the shots.",
             json!({ "clip_id": { "type": "string", "description": "The clip to split" } }),
             vec!["clip_id"]),
        tool("klipp_track_surface",
             "Follow a flat surface in a clip (a phone or laptop screen, a monitor, a sign, a book \
              cover) in perspective through the clip, so a picture can be pinned to it with the pin \
              op. Give click, a point inside it as fractions of the frame, and its corners are found \
              from the edges round it; or give its four corners on one frame, top left, top right, \
              bottom right, bottom left, as fractions of the frame: read them off klipp_frame_at at at_secs. Runs in the \
              background; the surface then shows under the clip in klipp_list_clips with its id and \
              the stretch it was followed over, one undo step. The track stops where the surface is \
              lost (covered, out of frame, blurred) rather than drifting: look at frames near there. \
              To continue past where it was lost, put the corners right on a frame there and track \
              the same surface again with direction forward (or backward): the new stretch is laid \
              over the old track and the rest kept.",
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
              grade and effects in a word, its id, the transitions on each track, and \
              the markers. Read this before \
              planning an edit; klipp_list_clips is the same as JSON with more fields.",
             json!({ "max_chars": { "type": "integer", "description": "Cut on a line boundary at this many characters (default 12000)" } }),
             vec![]),
        tool("klipp_analyze",
             "Find what is in the footage, in the background: shot boundaries (where the \
              picture cuts), EBU R128 loudness and true peak, the spans of silence, and \
              the transcript with word timestamps and the language when the speech model \
              is downloaded, the beat sheet of a screen recording, and its on-screen text \
              (OCR) when those models are downloaded. Those two run only on a screen recording: a file \
              with no camera metadata, or one tagged screen (klipp_tag_media) to ask for them. \
              One media item by id, or every file in the pool when \
              media_id is omitted. Pass wait true to wait for completion, then read \
              klipp_media_analysis. Items already analysed cost nothing. Runs on import \
              too, without downloading anything: pass download_models to fetch the \
              missing models once, with the user's knowledge — the speech model is \
              large (hundreds of MB), the OCR pair about 10 MB; each is only fetched \
              when its feature is on in preferences.",
             json!({
                 "media_id": { "type": "string", "description": "From klipp_list_media; omit for every item" },
                 "wait": { "type": "boolean", "description": "Wait until all requested items end; inspect finished and succeeded" },
                 "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour; timeout does not cancel analysis" },
                 "download_models": { "type": "boolean", "description": "Fetch missing models first (the user should know a download happens)" },
             }),
             vec![]),
        tool("klipp_search_transcripts",
             "Find where something is said: every transcript segment containing the query, \
              case-insensitively, with its media item and its start and end in the file.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" } }),
             vec!["query"]),
        tool("klipp_screen_text",
             "What was on the screen of a screen recording and when: every line OCR read, \
              folded into spans with the seconds it stayed up and where on the frame \
              (x, y, w, h as fractions), between from_secs and to_secs. Says which screen a \
              beat shows — \"Search & Filtering\" up 12–18 s — so a plan can name the right \
              moment. SENSITIVE lines (an email, a key, a password, a card number) are listed \
              masked with where to blur. Needs the two OCR models (about 10 MB): if the reply \
              says they are not downloaded, ask the user before klipp_analyze with \
              download_models true.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "all": { "type": "boolean", "description": "every line read, not the summary of headings and lines that stayed up" } }),
             vec!["media_id"]),
        tool("klipp_grade_to_target",
             "Grade every shot of the top picture track (or one clip) to the brief's look, by \
              measurement: each shot's frame is read in CIELAB (black, white and mid lightness, \
              contrast, chroma, the cast on its neutrals, and every face's skin hue), judged \
              against the look's numbers, and the primaries are moved toward the target — lift \
              from the black point, gain from the white, exposure from the median, temperature \
              and tint from the cast, saturation from the chroma — then read again, up to three \
              rounds. A dry run reports where every shot stands and its first step; apply true \
              grades them, the whole pass as one undo step. A screen recording is held only to its cast \
              and to clipping. Then klipp_skin_balance for a face still off the band, and a \
              look (klipp_look) for style on top.",
             json!({ "clip_id": { "type": "string" }, "apply": { "type": "boolean" }, "rounds": { "type": "integer" }, "force": { "type": "boolean" } }), vec![]),
        tool("klipp_audio_spectrum",
             "A file's sound as frequency against time: where a hum starts, when a hiss creeps \
              in, which stretch has the air conditioning in it. The scrolling analyser in the \
              app shows what is playing; this reads the whole file at once, which is the only \
              way to answer *when*. Answers with named findings and the band levels a second at \
              a time -- rumble, hum, voice, sibilance, hiss -- each judged relative to the \
              voice's own level rather than as an absolute, because a quiet recording of a clean \
              room and a loud one of a noisy room have the same noise floor and different \
              problems.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" } }), vec!["media_id"]),
        tool("klipp_adopt_look",
             "Take a look's numbers off a reference -- a file in the media pool, or the \
              timeline's own best-graded shot -- and make them the target every colour \
              judgement is held to. This is how a grade is briefed on a real job: somebody \
              points at a picture and says \"like this\". The named presets are the fallback \
              for when there is no such picture, not the normal path. apply false reports what \
              it measured and which preset is nearest; apply true writes it to the brief, and \
              check, grade_to_target and match_scenes all read it from there.",
             json!({ "media_id": { "type": "string" }, "from_timeline": { "type": "boolean" }, "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_match_scenes",
             "Group the timeline's shots into scenes by how they were lit, pick the shot in each \
              that is already closest to the look as the hero, grade the rest onto it, and report \
              how far each still is. This is the job that fills most of a colourist's day and it \
              is not the same as grading each shot well: two shots can each be defensible alone \
              and still cut badly, because the eye tracks a change across a cut far more readily \
              than an absolute error inside a shot. Matched on the face first, then exposure, \
              then the cast on the neutrals, then contrast. apply false reports; apply true \
              grades, the whole pass as one undo step.",
             json!({ "apply": { "type": "boolean" }, "rounds": { "type": "integer" } }), vec![]),
        tool("klipp_grade_chain",
             "A clip's grade chain in the order the picture flows through it, each node named \
              with the stage it belongs to -- input transform, balance, secondary, look, output \
              -- and whatever the chain does out of that order. A balance sitting after a look \
              is correcting the look's own cast and will never settle; a key pulled before the \
              balance drifts as the balance moves under it. Read this when a grading loop is \
              not converging.",
             json!({ "clip_id": { "type": "string" } }), vec![]),
        tool("klipp_colour_selfcheck",
             "Send a published 24-patch ColorChecker through the render path and read it back, \
              to find out whether the path itself moves colour. Run it once before grading to \
              measured targets: if the chart does not come back where it went in, every cast \
              and skin-tone reading is partly a measurement of the pipeline, and a grading loop \
              will chase it for ever. Checks the CPU and GPU paths and holds them against each \
              other, because the viewer shows one and an export may take the other.",
             json!({ "size": { "type": "integer" } }), vec![]),
        tool("klipp_measure_region",
             "One box of the frame read on its own, in the same numbers as klipp_frame_stats: a \
              face against its background, a sky, a product, a corner of a screen. x, y, w and h \
              are fractions of the frame. Use it when the whole-frame reading is the average of \
              two different things.",
             json!({ "secs": { "type": "number" }, "x": { "type": "number" }, "y": { "type": "number" }, "w": { "type": "number" }, "h": { "type": "number" }, "timeout_ms": { "type": "integer" } }), vec!["secs", "x", "y", "w", "h"]),
        tool("klipp_skin_balance",
             "Skin onto the skin band, by measurement: the faces in the clip's frame (the framing \
              analysis's people, the head of each) are read as an 8×8 grid of skin patches in \
              CIELAB; the face's median hue is held to the band 40–65° that real skin of every \
              kind sits in. With apply true a skin secondary (a qualifier keyed on skin, a hue \
              shift) turns the hue onto the band, measured again after each of up to three \
              rounds. Refused when the face is lit by two colours of light; when the scene's \
              neutrals carry the same cast the reply says to white balance first. Read a face \
              at any time with klipp_frame_stats.",
             json!({ "clip_id": { "type": "string" }, "apply": { "type": "boolean" } }), vec!["clip_id"]),
        tool("klipp_review",
             "The cut as an editor scrubs it, in one call: a draft render of the whole timeline \
              (max_width 640 by default, fast), a contact sheet of the draft (frames tiles in cols \
              columns, one every couple of seconds), and the check with its colour pass. Read the \
              sheet image for the shape of the cut — the rhythm, the titles, the cards — and the \
              check for what to fix; frame_at for a closer look at one frame. Takes as long as the \
              draft render (seconds to a minute).",
             json!({ "max_width": { "type": "integer" }, "frames": { "type": "integer" }, "cols": { "type": "integer" }, "path": { "type": "string" } }), vec![]),
        tool("klipp_narrate",
             "Narration from text, made on the machine. voice is a manifest id; the default is \
              kokoro_voice_af_heart (natural, female, US English; Apache-2.0). The other Kokoro voices: \
              kokoro_voice_af_bella, kokoro_voice_af_nicole, kokoro_voice_am_michael, kokoro_voice_am_fenrir, \
              kokoro_voice_am_puck (US); kokoro_voice_bf_emma, kokoro_voice_bf_isabella, kokoro_voice_bm_george, \
              kokoro_voice_bm_fable (UK). They need the espeak-ng program and one 87 MB download for all ten. \
              Lighter, more robotic Piper voices (the piper program): piper_en_us_ljspeech_high, \
              piper_en_us_kristin_medium, piper_en_us_norman_medium, piper_en_us_john_medium, \
              piper_en_us_libritts_high (CC BY: credit LibriTTS), piper_en_gb_cori_high, piper_sv_se_nst_medium \
              (Swedish); piper_en_us_lessac_medium is research only, never for paid work. Where Kokoro cannot \
              run, the default is piper_en_us_ljspeech_high. Nothing downloads unless download_models is true \
              (or the person downloads it in Window > Narrate); a refusal names the size. The take is \
              imported into the pool, and with apply true placed on a Voice \
              track at at_secs as dialogue. speed (0.5 to 2, 1 as the voice speaks; 0.85 for an \
              unhurried read) and pause_secs (the silence between sentences, 0 to 3) shape the \
              delivery. Then duck the music under it and transcript gives the \
              words with their times. A scratch voice to cut to; replace the file with a recorded \
              one later.",
             json!({ "text": { "type": "string" }, "at_secs": { "type": "number" }, "voice": { "type": "string" }, "speed": { "type": "number" }, "pause_secs": { "type": "number" }, "download_models": { "type": "boolean" }, "apply": { "type": "boolean" }, "path": { "type": "string" } }), vec!["text"]),
        tool("klipp_record",
             "Record the screen for secs with the recorder on the machine (gpu-screen-recorder, else \
              wf-recorder): target \"focused\" (the focused window, the default), \"screen\", a \
              monitor's name, or a region \"WxH+X+Y\"; fps 60. Imported into the pool unless import \
              is false, so the footage of the product can be made from here: ask the user to bring \
              the window up, record, analyze, cut.",
             json!({ "secs": { "type": "number" }, "target": { "type": "string" }, "fps": { "type": "integer" }, "path": { "type": "string" }, "import": { "type": "boolean" } }), vec!["secs"]),
        tool("klipp_auto_sfx",
             "Sound design from the cut, as one undo step with apply true (a preview without): \
              a whoosh on cuts, a click on reveals, a riser into the last cut. Optional media \
              ids choose your effects. With no ids, previews short built-in whooshes; apply \
              synthesizes and imports the sound automatically. Conflicting placements are \
              skipped so a dense montage does not abort the sound pass. Effects go on the \
              SFX track, marked sfx, at -6/-8 dB. Existing clips are preserved.",
             json!({ "whoosh": { "type": "string" }, "click": { "type": "string" }, "riser": { "type": "string" }, "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_auto_redact",
             "Hide everything the screen text found sensitive — emails, keys, passwords, card \
              numbers — wherever the timeline shows it: one redact op per finding (a cropped, \
              pixelated copy of the footage on a video track above), resolved as a plan. A dry \
              run by default; with apply true it is done as one undo step. Needs the OCR pass \
              (klipp_analyze with the OCR models). Look at a frame afterwards to see each box \
              covers its text.",
             json!({ "apply": { "type": "boolean" } }), vec![]),
        tool("klipp_search_screen_text",
             "Find every moment a word or phrase was on screen, across all footage: the \
              screen recording's Cmd-F. Each hit has the media item, the seconds it stayed \
              up, the text and where on the frame.",
             json!({ "query": { "type": "string" }, "max_hits": { "type": "integer" } }),
             vec!["query"]),
        tool("klipp_analysis_queue", "Put media_id first in the pending analysis queue. With cancel_pending true, cancel other queued items (all if no ID). Running work finishes normally. Call analyze first for an item that has no active request.",
             json!({ "media_id": { "type": "string" }, "cancel_pending": { "type": "boolean" } }), vec![]),
        tool("klipp_analysis_status", "Where analysis stands. With wait true, holds the response until the selected items end or timeout. Reports finished, succeeded and timed_out; failed items carry errors.",
             json!({ "wait": { "type": "boolean" }, "media_id": { "type": "string" }, "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour; does not cancel work" } }), vec![]),
        tool("klipp_media_analysis",
             "What is known about a media item, as text: its shots with their ranges, \
              its loudness, range and true peak, and its silences. The timeline view \
              shows the same facts per clip in brief. Times are seconds from the file's \
              start; a clip's source_in maps them to the timeline.",
             json!({ "media_id": { "type": "string" }, "max_chars": { "type": "integer", "description": "Cut at this many characters (default 8000)" } }),
             vec!["media_id"]),
        tool("klipp_beat_sheet",
             "The screencast/demo beat sheet of a media item: where the picture is \
              settled versus changing (idle/action segments), the candidate cut points \
              (a scene change, or where a change just settled — cut TO a settle and hold \
              until it reads, never mid-action), and the title/hold windows (settled \
              stretches long enough to hold a title without it fighting a moving picture). \
              Built from the shots + motion passes — run analyze first. With the OCR models \
              downloaded, every settled beat and reveal is named by what the screen showed \
              (\"6.13-14.50 s SEARCH & FILTERING\"), so a plan can be made by feature name. \
              Coarse (per-second): treat the times as where to LOOK, then confirm with \
              frame_at/contact_sheet where it matters. Ideal for cutting a screen recording \
              into a promo without cutting mid-feature.",
             json!({ "media_id": { "type": "string" } }),
             vec!["media_id"]),
        tool("klipp_find_shots",
             "Motion filters require decoded measurements; missing bins do not count as static. Find shots across every analysed media item: by framing (close-up, medium \
              close-up, medium, wide, very wide, inset — a small figure in a corner, a face \
              cam over a screen — or no people; B-roll wants no people), at least so many people, an \
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
              the whole file by default; for still images this is the hold range and may exceed the import duration. Its sound comes along linked on the first audio track \
              (made if none) unless with_audio is false (B-roll over a voice); an empty sequence \
              takes the first picture's size and rate; with insert, every unlocked track is \
              rippled later to make room and a clip straddling the anchor is split.\n\
              add_track {kind, name?} — a video or text track on top (it becomes track 0, every \
              other index moves down by one), an audio or subtitle track at the bottom.\n\
              remove_track {track, with_clips?} — a track out (index or kind), every track below moving \
              up by one; refused while it holds clips unless with_clips, which takes them and unlinks \
              their partners elsewhere.\n\
              captions {track?, from?, to?, max_chars?, max_lines?, replace?} — captions for what is heard \
              on the timeline, from the transcripts (run klipp_analyze first): each word lands \
              where its clip puts it, cues close at sentences, pauses and cuts, two lines of 42 \
              by default; on the first subtitle track, made at the bottom if there is none. The \
              reply lists every cue to be read; correct one with set_text. Do it after the \
              speech is placed and trimmed; after a re-cut, make them again with replace true.\n\
              audio {clip_id, volume_db?, gain_db?, pan?, fade_in?, fade_out?} — a clip's sound: \
              volume and pre-fader gain in dB, pan -1..1, fades in seconds; what is left out \
              stays. For an audio clip linked to a video clip the settings go on the video \
              clip, where playback and the render read them.\n\
              duck {clip_id, by_db?, fade_secs?} — music under speech: the clip's volume comes \
              down (9 dB by default) wherever speech is heard over it (not clips marked music, \
              sfx, ambience, foley or broll, nor any set 12 dB under the music; a transcribed clip \
              only where its words are, a voiceover throughout), moving over \
              fade_secs (0.5) each way, as volume keyframes. Set the music's volume first, \
              place the speech first.\n\
              volume {clip_id, keys:[{at, db}]} — a clip's volume drawn by hand: levels in dB from \
              the clip's own level (0 as set, -9 a duck, -60 near silence) at timeline seconds, straight moves between; \
              replaces its volume keyframes. For the dip duck cannot find, or a swell.\n\
              grade {clip_id, exposure?, contrast?, saturation?, temperature?, tint?, lift?, gamma?, \
              gain?, highlights?, shadows?, vibrance?, reset?} — a picture clip's primary grade: \
              what is given is set, the rest stays. Every knob is 0 at neutral except saturation \
              and contrast, which are multipliers with 1 as shot (1.1 a touch more, 0.9 less); exposure in stops; temperature/tint the white balance in mireds, -50 to 50 (about 5 Lab b* at 25; a tenth of a unit does nothing); \
              lift/gamma/gain the shadows/mids/highlights; gain and highlights are strong for their range, \
              so a first touch is ±0.02 to 0.05 (on a sunlit shot highlights -0.2 took the white point from 84 to 65 IRE, \
              and gain +0.05 clipped a sixth of the frame): step small and read frame_stats between; \
              vibrance is skin-safe saturation; \
              reset takes it to neutral first. Read frame_stats and grade to the numbers; grade \
              for continuity first — shots that follow each other should match — then the look.\n\
              grade_group {group, clips:[clip_id…], leave?} — picture clips into a named grade group \
              (made when new; a clip is in one group at most), or out of it with leave; \
              remove_grade_group {group} ends one, its clips keep their own grades. A group's clips \
              share a pre-clip grade (before each one's own: a camera's or a scene's balance) and a \
              post-clip grade (after it: the scene's look). grade and look take on in place of \
              clip_id: on {group, stage?: \"pre\"|\"post\" (default)} grades the group once for \
              every member; on \"timeline\" grades the whole sequence after every clip. One look \
              over a scene is a group and one look on it, not the same look on every shot.\n\
              slip {clip_id, secs} — move what a clip shows, not the clip: source in and out both by \
              secs (later in the file for positive), the timeline untouched; its linked sound slips by as \
              much, a J or L cut kept. slide {clip_id, secs} — the clip moved between its neighbours, \
              its picture and length kept: the one before longer, the one after shorter, sound with them; \
              refused where a neighbour runs out. role {clip_id, role} — \
              what a clip is to the mix and the checks: dialogue, music, sfx, ambience, foley, \
              voiceover, broll, graphics; a bed marked music is never taken for speech. matte \
              {clip_id, source?, channel?, invert?, feather_px?, choke_px?, show_source?, clear?} — \
              the clip seen only through another (track matte); without source, the clip above it: \
              title {text, size 700-1000, font \"Bebas Neue\", over_faces: true} over a clip, then matte on the clip, \
              puts the footage inside the word. format \
              {width, height, fps?} — the sequence's picture size (and rate): the delivery's, \
              the picture fitted, never stretched.\n\
              redact {clip_id, x, y, w, h, from?, to?, kind?, amount?} — hide a patch of the picture \
              (an email, a key, a face) for a stretch: a cropped copy pixelated (kind mosaic, \
              default) or blurred on a video track above; x, y, w, h are fractions of the frame \
              from the top left, as check's sensitive_on_screen and klipp_screen_text give them; \
              from and to default to the clip's edges. auto_redact does every finding at once.\n\
              pin {clip_id, media_id, surface?, from?, to?} — a picture, video or still onto a surface \
              klipp_track_surface followed in the clip: on a video track above, its frame on the \
              surface's corners and following them; surface is its id or name, the clip's latest when \
              left out.\n\
              effect {clip_id, kind, amount?} — a finishing effect on a picture: vignette, grain, glow, \
              bloom, diffusion, halation, light_rays, lens_flare, anamorphic, blur, radial_blur, \
              directional_blur, sharpen, chromatic_aberration, tilt_shift, mosaic, posterize, invert, \
              mirror, lens_distortion, edge_detect, denoise, vhs, old_film, corners (rounded alpha mask), \
              shadow (drop shadow), outer_glow (glow outside the layer silhouette), fill_bars (the black \
              bars round an upright phone shot in a wide frame filled with the picture, enlarged and \
              blurred: Resolve's blanking fill; more amount is softer and darker); amount 0-1. These are \
              effect kinds, e.g. {op: effect, clip_id: ID, kind: corners, amount: 0.4}; shadow and outer_glow \
              use the same fields. Place the picture with compose to show the backdrop around it. clean_audio {clip_id, voice?, reduction_db?, wind?, hiss?, hum?, deess?} — clean a \
              clip's sound, each flag its tool: voice isolates dialogue from wind, traffic and room noise with the \
              Voice Isolation AI effect, the best cleanup where someone speaks (reduction_db 12 keeps the place, \
              default 24, 60 removes all it can; leave it off where nobody speaks, it would take the ambience); \
              wind for gusts and rumble, hiss for tape or air hiss, hum for mains buzz, deess for harsh sibilance. \
              stabilize {clip_id, smoothing?} — steady a shaky clip (0-1, crops a little). \
              retime {clip_id, speed, ripple?} — change speed, its linked sound with it (>1 faster/shorter, <1 slower/longer, negative reversed; the sound plays at the speed, its pitch moving with it, backward when reversed; a speed ramp's sound is silent); ripple moves the clips after it, without it a clip that would grow into the next is refused and a \
              shrunk one leaves a gap. \
              generator {data, at, secs?|end?, track?} — place native generator data (see klipp_generator), \
              with exactly one duration secs or end anchor. Omit track for a Background video track below all \
              existing layers; e.g. {op: generator, data: {SolidColor: {color: [16,24,32,255]}}, at: {secs: 0}, secs: 3}.\n\
              compose {clip_id, poses: [{secs?, x?, y?, width?, height?, rotation_deg?, opacity?}]} — place a \
              picture inside the frame; one pose holds, more animate. secs is relative to the clip; x,y are \
              its centre as frame fractions (-1..2, default 0.5); width and height must be equal (0.01..4, \
              default 1); rotation_deg is -360..360 (default 0), opacity 0..1 (default 1). Example: \
              {op: compose, clip_id: ID, poses: [{x: 0.7, y: 0.7, width: 0.4, height: 0.4}]}.\n\
              camera {keys: [{secs?, x?, y?, zoom?, roll_deg?}]} — move the whole sequence, including all its \
              layers. secs is TIMELINE time within the piece (default 0); x,y are the camera's look point \
              (-1..2, default 0.5), zoom 0.1..8 (default 1), roll_deg -360..360 (default 0). One key holds; \
              keys: [] clears the camera. Example over a three-second piece: \
              {op: camera, keys: [{secs: 0}, {secs: 3, zoom: 1.1, x: 0.55}]}.\n\
              reframe {clip_id, scale?, x?, y?} — punch in and reposition a picture: scale zooms \
              (1 as shot, up to 4), x and y pan -1..1. Salvage a wide or badly-framed shot, \
              or set a slow push; it sits before other effects.\n\
              zoom {clip_id, from?, to, x?, y?} — an ANIMATED zoom over the clip: scale from→to \
              (1 as shot, up to 4), a push-in or pull-out, optionally moving toward x,y (-1..1) for \
              a Ken Burns move. Renders on the GPU; motion on a still shot.\n\
              pan {clip_id, keys: [{secs, scale?, x?, y?}]} — an animated crop through poses in \
              the clip's own seconds: scale (1-4) centred on frame point x,y (0..1, as the beat \
              sheet's spots and screen text give them); eases between, never past the edge.\n\
              follow {clip_id, scale?, hold_secs?} — a pan made from the beat sheet: push to each \
              spot where something happened in the clip's stretch, hold, ease back out.\n\
              highlight {at, secs?, x, y, w, h, kind? box|ring|spot, color?, stroke?, track?} — a \
              highlight over the region (centre x,y, size w,h, fractions of the frame): a rounded \
              box, a capsule ring, or everything outside it dimmed; fades in and out.\n\
              ramp {clip_id, from, to, secs?} — a speed ramp from×→to× over secs of timeline, then \
              holding to; the clip's length follows.\n\
              qualify {clip_id, target?|hue?, hue_width?, sat_low?, sat_high?, lum_low?, lum_high?, \
              invert?, exposure?, saturation?, hue_shift?, temperature?, remove?, window?, \
              window_feather?} — a secondary: key a colour or tone (target skin/sky/foliage/reds…, \
              or a hue in degrees, with saturation and luminance windows) and correct only it \
              (exposure, saturation, hue_shift, temperature); invert grades all but the key. \
              window holds the key inside a shape as well — a list of {at, x, y, w, h}, boxes in \
              frame fractions at times in clip seconds — because a hue key alone cannot tell a \
              face from a red jumper, a brick wall or a wooden table, and turns all of them. \
              Several entries keyframe the window so it follows a subject who moves. Fix a sky, \
              calm a loud shirt, warm skin.\n\
              transition {clip_id, kind?, secs?} — a transition between a clip and the one after \
              it on its track, secs long (0.5 default), centred on the cut. kind is cross_dissolve \
              (the default, and all an interview or documentary wants), dip_to_black, dip_to_white, \
              or a stylised one (zoom, whip, glitch, film_burn) a piece asks for by its style, \
              never by default. Both clips need footage beyond the cut to overlap.\n\
              style {clip_id | track, from?, to?, size?, color?, font?, bold?, align?, x?, y?, backing?, outline?, full_frame?, shadow?} \
              — the look and place of one title, or of every title on a track (\"subtitle\" for \
              all the captions), between from and to when given: size in pixels of a 1080-line \
              frame, color #rrggbb, x and y where the text's centre goes (0 to 1), backing how \
              dark a panel sits behind it (0 none to 1 black), outline a width in pixels. It is \
              how captions are lifted clear of a lower third while it is up.\n\
              lower_third {name, role?, at, secs?, track?, side?, x?, y?} — a name with a role \
              under it, bottom left in title-safe, four seconds by default, fading in and out, on \
              a text track with room (made on top if none: every other index then moves down by \
              one). Refused across a shot change. Names come from the brief, never a guess. Where \
              the speaker is not on the left or the picture has its own name burnt in there, \
              side \"right\" sets it against the right edge, x is that edge and y the name's line, \
              as fractions of the frame inside title-safe.\n\
              vector_path {clip_id, object_id?, path:{pose:{contours:[{closed,vertices:[{point:[x,y],incoming:[x,y],outgoing:[x,y]}]}],fill:[r,g,b,a],stroke:[r,g,b,a],stroke_width,stroke_glow?:{radius,strength}},keyframes:[{time_secs,pose,easing}],trim?:{range:{start,end,offset},keyframes:[{time_secs,range,easing}]}}} edits a title layer; trim uses independent normalized arc-length keys per contour, open paths clip and closed paths wrap offset, fill unchanged, round caps/joins, glow from revealed stroke only; coordinates normalized to its box, keyed topology must match. title {text, at, style? (a brief type style or heading|callout|card), secs?, track?, x?, y?, size?, color?, font?, bold?, align?, backing?, \
              backing_color?, full_frame?, outline?, outline_color?, glow?, glow_color?, gradient?, \
              animate?, animate_secs?, stagger?, enter?, exit?, loop?, glitch?, chromatic?, shadow?} — a title from scratch, enter/exit are {kind, duration_secs?, delay_secs?, easing?, stagger?, stagger_secs?, distance?}. \
              Kinds include mask_rise/mask_drop and none (disable); easing is linear, out_cubic, out_quint, out_expo, out_back, out_bounce, in_out_cubic or in_cubic. \
              Timing: duration (0,5] s, delay [0,3600] s, stagger [0,60] s; distance [0,100] font-size units, only for rise/drop/slide_left/slide_right. Entrance delay plus exit lead must leave a visible interval inside the title. Timing-only overrides on a disabled entrance are ignored. \
              Exit delay is lead time before clip end; exit stagger runs in the renderer's reverse unit order. Masked travel stays fixed. \
              Do not combine enter with animate/animate_secs/stagger. Explicit enter/exit replaces that style side; omitted sides inherit. \
              any look and motion: x,y centre (0..1), size in px of a 1080 frame, color #rrggbb; \
              backing a dark panel (0..1) or backing_color, full_frame a solid card, outline+outline_color \
              a stroke, glow+glow_color a neon glow, shadow {x?, y?, blur?, color?, opacity?} a soft drop shadow (px of a 1080 frame; opacity 0 removes it), gradient [#top,#bottom]; animate an entrance (fade, \
              rise, drop, slide_left, slide_right, scale_up, scale_down, blur, mask_rise, mask_drop, typewriter, pop, slam, fly_left, fly_right, fly_top, fly_bottom, bounce, flicker, focus_in, soft_rise) with stagger \
              (line/word/char); loop a continuous effect (pulse, flash, neon, wave, shake, color_cycle, scroll for a credits roll, crawl for a ticker; roll_shape {focus {band, edge_opacity}} thins a roll away from the middle; edge_fade {top, bottom, left, right} fades any title out towards those frame edges); \
              glitch and chromatic (0..1) for a digital edge.\n\
              set_text {clip_id, text} — the words of a title or a caption, its look, place and \
              timing kept: for a cue the transcript misheard or a name it cannot spell.\n\
              trim {clip_id, edge: \"in\"|\"out\", to, ripple?, linked?} — one edge to an anchor, the \
              other stays, the source moves with the edge; the clip it is linked to (a \
              picture's sound) is trimmed with it by the same amount unless linked is false; with ripple the later clips on the \
              track close up by the change (a head trim then keeps the clip's start).\n\
              split {clip_id, at} — into two, the linked sound with it.\n\
              join {clip_id, side?} — close a through edit: the clip and the piece of the same shot on its \
              side (next, default, or previous) made one again, its sound with it; name the false cut's \
              own neighbours, as every edit of one film runs on in its file.\n\
              remove {clip_id, ripple?, linked?} — off the track, its linked sound or picture with it unless linked is false; with ripple the later clips close the gap; a linked ripple refuses different timeline ranges or a locked partner. Use ripple:false to leave later clips in place.\n\
              cut {from, to} — that stretch out of every unlocked track, linked halves split with it, everything after moved up: a text-based delete. Refused through a retimed clip.\n\
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
                     "detail": { "type": "boolean", "description": "every step in full and each touched clip's range as JSON; off by default, when long runs of one kind of step are folded" } }),
             vec!["ops"]),
        tool("klipp_apply",
             concat!("Apply a list of editorial operations as ONE undo step, after resolving them the \
              way klipp_plan does; pass its expected_revision and optionally expected_project_identity \
              to refuse the whole edit if the timeline or project changed since planning. \
              Replies with the steps and the revision. ",
              "Editorial operations, resolved by Klipp into frame-exact edits. Give a list of ops; \
              each is an object with \"op\" and its fields:\n\
              place {media_id, track, at, source_in?, source_out?, insert?, with_audio?} — a \
              media item onto a track (an index from klipp_timeline, or \"video\"/\"audio\"/\"text\" \
              for the first of that kind) at an anchor; source_in/out in seconds of the file, \
              the whole file by default; for still images this is the hold range and may exceed the import duration. Its sound comes along linked on the first audio track \
              (made if none) unless with_audio is false (B-roll over a voice); an empty sequence \
              takes the first picture's size and rate; with insert, every unlocked track is \
              rippled later to make room and a clip straddling the anchor is split.\n\
              add_track {kind, name?} — a video or text track on top (it becomes track 0, every \
              other index moves down by one), an audio or subtitle track at the bottom.\n\
              remove_track {track, with_clips?} — a track out (index or kind), every track below moving \
              up by one; refused while it holds clips unless with_clips, which takes them and unlinks \
              their partners elsewhere.\n\
              captions {track?, from?, to?, max_chars?, max_lines?, replace?} — captions for what is heard \
              on the timeline, from the transcripts (run klipp_analyze first): each word lands \
              where its clip puts it, cues close at sentences, pauses and cuts, two lines of 42 \
              by default; on the first subtitle track, made at the bottom if there is none. The \
              reply lists every cue to be read; correct one with set_text. Do it after the \
              speech is placed and trimmed; after a re-cut, make them again with replace true.\n\
              audio {clip_id, volume_db?, gain_db?, pan?, fade_in?, fade_out?} — a clip's sound: \
              volume and pre-fader gain in dB, pan -1..1, fades in seconds; what is left out \
              stays. For an audio clip linked to a video clip the settings go on the video \
              clip, where playback and the render read them.\n\
              duck {clip_id, by_db?, fade_secs?} — music under speech: the clip's volume comes \
              down (9 dB by default) wherever speech is heard over it (not clips marked music, \
              sfx, ambience, foley or broll, nor any set 12 dB under the music; a transcribed clip \
              only where its words are, a voiceover throughout), moving over \
              fade_secs (0.5) each way, as volume keyframes. Set the music's volume first, \
              place the speech first.\n\
              volume {clip_id, keys:[{at, db}]} — a clip's volume drawn by hand: levels in dB from \
              the clip's own level (0 as set, -9 a duck, -60 near silence) at timeline seconds, straight moves between; \
              replaces its volume keyframes. For the dip duck cannot find, or a swell.\n\
              grade {clip_id, exposure?, contrast?, saturation?, temperature?, tint?, lift?, gamma?, \
              gain?, highlights?, shadows?, vibrance?, reset?} — a picture clip's primary grade: \
              what is given is set, the rest stays. Every knob is 0 at neutral except saturation \
              and contrast, which are multipliers with 1 as shot (1.1 a touch more, 0.9 less); exposure in stops; temperature/tint the white balance in mireds, -50 to 50 (about 5 Lab b* at 25; a tenth of a unit does nothing); \
              lift/gamma/gain the shadows/mids/highlights; gain and highlights are strong for their range, \
              so a first touch is ±0.02 to 0.05 (on a sunlit shot highlights -0.2 took the white point from 84 to 65 IRE, \
              and gain +0.05 clipped a sixth of the frame): step small and read frame_stats between; \
              vibrance is skin-safe saturation; \
              reset takes it to neutral first. Read frame_stats and grade to the numbers; grade \
              for continuity first — shots that follow each other should match — then the look.\n\
              grade_group {group, clips:[clip_id…], leave?} — picture clips into a named grade group \
              (made when new; a clip is in one group at most), or out of it with leave; \
              remove_grade_group {group} ends one, its clips keep their own grades. A group's clips \
              share a pre-clip grade (before each one's own: a camera's or a scene's balance) and a \
              post-clip grade (after it: the scene's look). grade and look take on in place of \
              clip_id: on {group, stage?: \"pre\"|\"post\" (default)} grades the group once for \
              every member; on \"timeline\" grades the whole sequence after every clip. One look \
              over a scene is a group and one look on it, not the same look on every shot.\n\
              slip {clip_id, secs} — move what a clip shows, not the clip: source in and out both by \
              secs (later in the file for positive), the timeline untouched; its linked sound slips by as \
              much, a J or L cut kept. slide {clip_id, secs} — the clip moved between its neighbours, \
              its picture and length kept: the one before longer, the one after shorter, sound with them; \
              refused where a neighbour runs out. role {clip_id, role} — \
              what a clip is to the mix and the checks: dialogue, music, sfx, ambience, foley, \
              voiceover, broll, graphics; a bed marked music is never taken for speech. matte \
              {clip_id, source?, channel?, invert?, feather_px?, choke_px?, show_source?, clear?} — \
              the clip seen only through another (track matte); without source, the clip above it: \
              title {text, size 700-1000, font \"Bebas Neue\", over_faces: true} over a clip, then matte on the clip, \
              puts the footage inside the word. format \
              {width, height, fps?} — the sequence's picture size (and rate): the delivery's, \
              the picture fitted, never stretched.\n\
              redact {clip_id, x, y, w, h, from?, to?, kind?, amount?} — hide a patch of the picture \
              (an email, a key, a face) for a stretch: a cropped copy pixelated (kind mosaic, \
              default) or blurred on a video track above; x, y, w, h are fractions of the frame \
              from the top left, as check's sensitive_on_screen and klipp_screen_text give them; \
              from and to default to the clip's edges. auto_redact does every finding at once.\n\
              pin {clip_id, media_id, surface?, from?, to?} — a picture, video or still onto a surface \
              klipp_track_surface followed in the clip: on a video track above, its frame on the \
              surface's corners and following them; surface is its id or name, the clip's latest when \
              left out.\n\
              effect {clip_id, kind, amount?} — a finishing effect on a picture: vignette, grain, glow, \
              bloom, diffusion, halation, light_rays, lens_flare, anamorphic, blur, radial_blur, \
              directional_blur, sharpen, chromatic_aberration, tilt_shift, mosaic, posterize, invert, \
              mirror, lens_distortion, edge_detect, denoise, vhs, old_film, corners (rounded alpha mask), \
              shadow (drop shadow), outer_glow (glow outside the layer silhouette), fill_bars (the black \
              bars round an upright phone shot in a wide frame filled with the picture, enlarged and \
              blurred: Resolve's blanking fill; more amount is softer and darker); amount 0-1. These are \
              effect kinds, e.g. {op: effect, clip_id: ID, kind: corners, amount: 0.4}; shadow and outer_glow \
              use the same fields. Place the picture with compose to show the backdrop around it. clean_audio {clip_id, voice?, reduction_db?, wind?, hiss?, hum?, deess?} — clean a \
              clip's sound, each flag its tool: voice isolates dialogue from wind, traffic and room noise with the \
              Voice Isolation AI effect, the best cleanup where someone speaks (reduction_db 12 keeps the place, \
              default 24, 60 removes all it can; leave it off where nobody speaks, it would take the ambience); \
              wind for gusts and rumble, hiss for tape or air hiss, hum for mains buzz, deess for harsh sibilance. \
              stabilize {clip_id, smoothing?} — steady a shaky clip (0-1, crops a little). \
              retime {clip_id, speed, ripple?} — change speed, its linked sound with it (>1 faster/shorter, <1 slower/longer, negative reversed; the sound plays at the speed, its pitch moving with it, backward when reversed; a speed ramp's sound is silent); ripple moves the clips after it, without it a clip that would grow into the next is refused and a \
              shrunk one leaves a gap. \
              generator {data, at, secs?|end?, track?} — place native generator data (see klipp_generator), \
              with exactly one duration secs or end anchor. Omit track for a Background video track below all \
              existing layers; e.g. {op: generator, data: {SolidColor: {color: [16,24,32,255]}}, at: {secs: 0}, secs: 3}.\n\
              compose {clip_id, poses: [{secs?, x?, y?, width?, height?, rotation_deg?, opacity?}]} — place a \
              picture inside the frame; one pose holds, more animate. secs is relative to the clip; x,y are \
              its centre as frame fractions (-1..2, default 0.5); width and height must be equal (0.01..4, \
              default 1); rotation_deg is -360..360 (default 0), opacity 0..1 (default 1). Example: \
              {op: compose, clip_id: ID, poses: [{x: 0.7, y: 0.7, width: 0.4, height: 0.4}]}.\n\
              camera {keys: [{secs?, x?, y?, zoom?, roll_deg?}]} — move the whole sequence, including all its \
              layers. secs is TIMELINE time within the piece (default 0); x,y are the camera's look point \
              (-1..2, default 0.5), zoom 0.1..8 (default 1), roll_deg -360..360 (default 0). One key holds; \
              keys: [] clears the camera. Example over a three-second piece: \
              {op: camera, keys: [{secs: 0}, {secs: 3, zoom: 1.1, x: 0.55}]}.\n\
              reframe {clip_id, scale?, x?, y?} — punch in and reposition a picture: scale zooms \
              (1 as shot, up to 4), x and y pan -1..1. Salvage a wide or badly-framed shot, \
              or set a slow push; it sits before other effects.\n\
              zoom {clip_id, from?, to, x?, y?} — an ANIMATED zoom over the clip: scale from→to \
              (1 as shot, up to 4), a push-in or pull-out, optionally moving toward x,y (-1..1) for \
              a Ken Burns move. Renders on the GPU; motion on a still shot.\n\
              pan {clip_id, keys: [{secs, scale?, x?, y?}]} — an animated crop through poses in \
              the clip's own seconds: scale (1-4) centred on frame point x,y (0..1, as the beat \
              sheet's spots and screen text give them); eases between, never past the edge.\n\
              follow {clip_id, scale?, hold_secs?} — a pan made from the beat sheet: push to each \
              spot where something happened in the clip's stretch, hold, ease back out.\n\
              highlight {at, secs?, x, y, w, h, kind? box|ring|spot, color?, stroke?, track?} — a \
              highlight over the region (centre x,y, size w,h, fractions of the frame): a rounded \
              box, a capsule ring, or everything outside it dimmed; fades in and out.\n\
              ramp {clip_id, from, to, secs?} — a speed ramp from×→to× over secs of timeline, then \
              holding to; the clip's length follows.\n\
              qualify {clip_id, target?|hue?, hue_width?, sat_low?, sat_high?, lum_low?, lum_high?, \
              invert?, exposure?, saturation?, hue_shift?, temperature?, remove?, window?, \
              window_feather?} — a secondary: key a colour or tone (target skin/sky/foliage/reds…, \
              or a hue in degrees, with saturation and luminance windows) and correct only it \
              (exposure, saturation, hue_shift, temperature); invert grades all but the key. \
              window holds the key inside a shape as well — a list of {at, x, y, w, h}, boxes in \
              frame fractions at times in clip seconds — because a hue key alone cannot tell a \
              face from a red jumper, a brick wall or a wooden table, and turns all of them. \
              Several entries keyframe the window so it follows a subject who moves. Fix a sky, \
              calm a loud shirt, warm skin.\n\
              transition {clip_id, kind?, secs?} — a transition between a clip and the one after \
              it on its track, secs long (0.5 default), centred on the cut. kind is cross_dissolve \
              (the default, and all an interview or documentary wants), dip_to_black, dip_to_white, \
              or a stylised one (zoom, whip, glitch, film_burn) a piece asks for by its style, \
              never by default. Both clips need footage beyond the cut to overlap.\n\
              style {clip_id | track, from?, to?, size?, color?, font?, bold?, align?, x?, y?, backing?, outline?, full_frame?, shadow?} \
              — the look and place of one title, or of every title on a track (\"subtitle\" for \
              all the captions), between from and to when given: size in pixels of a 1080-line \
              frame, color #rrggbb, x and y where the text's centre goes (0 to 1), backing how \
              dark a panel sits behind it (0 none to 1 black), outline a width in pixels. It is \
              how captions are lifted clear of a lower third while it is up.\n\
              lower_third {name, role?, at, secs?, track?, side?, x?, y?} — a name with a role \
              under it, bottom left in title-safe, four seconds by default, fading in and out, on \
              a text track with room (made on top if none: every other index then moves down by \
              one). Refused across a shot change. Names come from the brief, never a guess. Where \
              the speaker is not on the left or the picture has its own name burnt in there, \
              side \"right\" sets it against the right edge, x is that edge and y the name's line, \
              as fractions of the frame inside title-safe.\n\
              vector_path {clip_id, object_id?, path:{pose:{contours:[{closed,vertices:[{point:[x,y],incoming:[x,y],outgoing:[x,y]}]}],fill:[r,g,b,a],stroke:[r,g,b,a],stroke_width,stroke_glow?:{radius,strength}},keyframes:[{time_secs,pose,easing}],trim?:{range:{start,end,offset},keyframes:[{time_secs,range,easing}]}}} edits a title layer; trim uses independent normalized arc-length keys per contour, open paths clip and closed paths wrap offset, fill unchanged, round caps/joins, glow from revealed stroke only; coordinates normalized to its box, keyed topology must match. title {text, at, style? (a brief type style or heading|callout|card), secs?, track?, x?, y?, size?, color?, font?, bold?, align?, backing?, \
              backing_color?, full_frame?, outline?, outline_color?, glow?, glow_color?, gradient?, \
              animate?, animate_secs?, stagger?, enter?, exit?, loop?, glitch?, chromatic?, shadow?} — a title from scratch, enter/exit are {kind, duration_secs?, delay_secs?, easing?, stagger?, stagger_secs?, distance?}. \
              Kinds include mask_rise/mask_drop and none (disable); easing is linear, out_cubic, out_quint, out_expo, out_back, out_bounce, in_out_cubic or in_cubic. \
              Timing: duration (0,5] s, delay [0,3600] s, stagger [0,60] s; distance [0,100] font-size units, only for rise/drop/slide_left/slide_right. Entrance delay plus exit lead must leave a visible interval inside the title. Timing-only overrides on a disabled entrance are ignored. \
              Exit delay is lead time before clip end; exit stagger runs in the renderer's reverse unit order. Masked travel stays fixed. \
              Do not combine enter with animate/animate_secs/stagger. Explicit enter/exit replaces that style side; omitted sides inherit. \
              any look and motion: x,y centre (0..1), size in px of a 1080 frame, color #rrggbb; \
              backing a dark panel (0..1) or backing_color, full_frame a solid card, outline+outline_color \
              a stroke, glow+glow_color a neon glow, shadow {x?, y?, blur?, color?, opacity?} a soft drop shadow (px of a 1080 frame; opacity 0 removes it), gradient [#top,#bottom]; animate an entrance (fade, \
              rise, drop, slide_left, slide_right, scale_up, scale_down, blur, mask_rise, mask_drop, typewriter, pop, slam, fly_left, fly_right, fly_top, fly_bottom, bounce, flicker, focus_in, soft_rise) with stagger \
              (line/word/char); loop a continuous effect (pulse, flash, neon, wave, shake, color_cycle, scroll for a credits roll, crawl for a ticker; roll_shape {focus {band, edge_opacity}} thins a roll away from the middle; edge_fade {top, bottom, left, right} fades any title out towards those frame edges); \
              glitch and chromatic (0..1) for a digital edge.\n\
              set_text {clip_id, text} — the words of a title or a caption, its look, place and \
              timing kept: for a cue the transcript misheard or a name it cannot spell.\n\
              trim {clip_id, edge: \"in\"|\"out\", to, ripple?, linked?} — one edge to an anchor, the \
              other stays, the source moves with the edge; the clip it is linked to (a \
              picture's sound) is trimmed with it by the same amount unless linked is false; with ripple the later clips on the \
              track close up by the change (a head trim then keeps the clip's start).\n\
              split {clip_id, at} — into two, the linked sound with it.\n\
              join {clip_id, side?} — close a through edit: the clip and the piece of the same shot on its \
              side (next, default, or previous) made one again, its sound with it; name the false cut's \
              own neighbours, as every edit of one film runs on in its file.\n\
              remove {clip_id, ripple?, linked?} — off the track, its linked sound or picture with it unless linked is false; with ripple the later clips close the gap; a linked ripple refuses different timeline ranges or a locked partner. Use ripple:false to leave later clips in place.\n\
              cut {from, to} — that stretch out of every unlocked track, linked halves split with it, everything after moved up: a text-based delete. Refused through a retimed clip.\n\
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
             "Several write tools applied together in one frame and recorded as ONE undo \
              step: use it for anything that is one action in your plan (an append and \
              its grade, a set of parameter changes). Every call is checked before any \
              is applied; a bad one refuses the whole batch and nothing changes. Reads \
              (status, lists, screenshots) are refused inside a batch. klipp_look calls \
              batch too, but only with other looks: one look on many clips is one call \
              and one undo step. Replies with the document revision after the batch.",
             json!({ "calls": { "type": "array", "description": "In order. Each: {\"tool\": name, \"args\": {...}}",
                 "items": { "type": "object", "properties": {
                     "tool": { "type": "string" }, "args": { "type": "object" } }, "required": ["tool"] } } }),
             vec!["calls"]),
        tool("klipp_export",
             "Render the active sequence to a file with Klipp's default export settings; \
              the container comes from the extension (mp4, mov, mkv, webm). Refused when \
              a clip's media is offline or the path is one of the project's media files. \
              Asynchronous: call klipp_export_status with wait true to wait for completion; then the \
              file is done. The path must be absolute.",
             json!({
                 "path": { "type": "string", "description": "Absolute output path" },
                 "width": { "type": "integer" }, "height": { "type": "integer" },
                 "fps": { "type": "number" },
                 "crf": { "type": "integer", "description": "Quality, lower is better (default 23)" },
                 "target_lufs": { "type": "number", "description": "Loudness normalisation target, e.g. -14 for the web, -23 for broadcast; left out, the brief's" },
                 "burn_captions": { "type": "boolean", "description": "Captions and titles burnt into the picture; on unless you say otherwise. The reply says which it was, so you know whether a sidecar subtitle file is needed." },
             }),
             vec!["path"]),
        tool("klipp_export_status",
             "Every render job Klipp knows: status (queued, rendering, complete, failed, \
              cancelled), progress 0 to 1, frames, stage, error, loudness result. With wait true, waits for job_id or all currently known jobs. Returns finished, succeeded and timed_out; failed or cancelled is not success. Timeout does not cancel renders.",
             json!({ "wait": { "type": "boolean" }, "job_id": { "type": "string" }, "timeout_ms": { "type": "integer", "description": "Maximum wait, default one hour" } }), vec![]),
        tool("klipp_export_subtitles",
             "Write the sequence's subtitle and text clips as an SRT file at an absolute path.",
             json!({ "path": { "type": "string" } }), vec!["path"]),
        tool("klipp_transcript",
             "What is said in a media file between two times, with when, in seconds of the \
              file: the segments, and with words true every word with its start and end, and \
              the stretches measured silent in that range. Use it to put a cut on the edge of \
              a word or in a silence (a cut in measured silence is clean), and to read a \
              stretch the analysis text cut short.",
             json!({ "media_id": { "type": "string" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" }, "words": { "type": "boolean" } }),
             vec!["media_id"]),
        tool("klipp_cut_points",
             "The clean places to cut in a media file, ranked: the measured pauses (strength \
              3, the cleanest), the sentence ends and shot changes (strength 2), the phrase \
              breaks (strength 1). Pass near_secs and it also says whether that moment is \
              clean, mid-sentence or mid-word, and the nearest clean point to move to. Use it \
              before you trim or place a spoken clip, so a cut never lands through a word or \
              the middle of a sentence. from_secs and to_secs narrow the range.",
             json!({ "media_id": { "type": "string" }, "near_secs": { "type": "number" }, "from_secs": { "type": "number" }, "to_secs": { "type": "number" } }),
             vec!["media_id"]),
        tool("klipp_prepare_cut",
             "Prepare a frame-exact cut in a bounded TIMELINE range. Maps source pauses, word and sentence edges, shot changes, and settled screen reveals through trims and retiming onto sequence frames. Returns ranked candidates with evidence and uncertainty, relevant clips, linked sound, locks, handles, revision and project identity. Give the editing intent and an optional target moment. This only reads; choose a frame, plan the cut, and apply with its revision precondition. If ambiguous, inspect a contact sheet or audio excerpt first.",
             json!({ "from_secs": { "type": "number" }, "to_secs": { "type": "number" },
                     "near_secs": { "type": "number" }, "intent": { "type": "string" },
                     "max_candidates": { "type": "integer", "minimum": 1, "maximum": 32 } }),
             vec!["from_secs", "to_secs"]),
        tool("klipp_selects",
             "Cut a long take down to its selects: the runs of speech between the real pauses, \
              each with the time it runs from and to and what is said, marked keep or drop (a \
              run that is only filler — um, uh — or too short to use is a drop). Use it to build \
              a tight cut from a loose recording: place each keep range in order and close the \
              gaps. min_gap_secs sets the pause that starts a new select (0.6 by default). A \
              read; changes nothing.",
             json!({ "media_id": { "type": "string" }, "min_gap_secs": { "type": "number", "description": "the pause length that splits one select from the next, seconds" } }),
             vec!["media_id"]),
        tool("klipp_beats",
             "The tempo and beat grid of a clip's sound: BPM, a confidence, and the beat times \
              in seconds. Run it on the music you are cutting to, then place or trim cuts to a \
              beat time for a musical rhythm. A low confidence means no clear beat (speech or \
              ambience). A read; changes nothing.",
             json!({ "media_id": { "type": "string" } }), vec!["media_id"]),
        tool("klipp_style_profile",
             "Read a reference video's editing style as facts, so you can build a clip in the \
              same feel from other footage: its cut cadence (how many shots, how long, how many \
              per minute, and whether it cuts on the beat), its tempo, its look measured off the \
              picture through the scopes (blacks, whites, contrast, saturation, warmth), and its \
              motion and detected holds. Cut cadence is not animation pacing: inspect changes \
              within shots before choosing hold lengths. Motion statistics do not identify \
              semantic animation events. It reads only. Match the feel with your own footage \
              and your own or licensed music, never the reference's track. The reference must be \
              imported and analysed first.",
             json!({ "media_id": { "type": "string" } }), vec!["media_id"]),
        tool("klipp_search_music",
             "Search royalty-free music by mood or genre (e.g. \"upbeat electronic\", \"calm \
              piano\", \"cinematic\") from Jamendo. Each result shows its title, artist, length, \
              and licence — including whether it clears commercial use. Preview a result's \
              preview_url before committing, then add it with klipp_add_music. Needs a Jamendo \
              client id in the user's key config. Use the reference's feel (style_profile) to \
              pick music of matching energy — never the reference's own track.",
             json!({ "query": { "type": "string" }, "limit": { "type": "integer" }, "commercial_only": { "type": "boolean", "description": "only tracks whose licence allows commercial use" }, "instrumental": { "type": "boolean", "description": "only instrumental tracks, for a promo with its own titles" } }), vec!["query"]),
        tool("klipp_search_sfx",
             "Search royalty-free sound effects by keyword (e.g. \"whoosh\", \"door slam\", \"rain\", \
              \"impact\") from Freesound. Each result shows its licence and whether it clears \
              commercial use; preview a result's preview_url, then add it with klipp_add_music \
              (it takes a sound effect the same as a track). Good for transitions, cuts and \
              atmosphere. Needs a Freesound token in the user's key config.",
             json!({ "query": { "type": "string" }, "limit": { "type": "integer" } }), vec!["query"]),
        tool("klipp_add_music",
             "Download a searched music track OR sound effect into the media pool, keeping its licence and \
              attribution beside the file for your export credits. Pass the fields from a \
              klipp_search_music result (title, artist, download_url, license, license_url, \
              page_url). Then place it on an audio track, mark the track Music, and duck it \
              under speech. Honour the licence — keep non-commercial tracks out of commercial work.",
             json!({ "title": { "type": "string" }, "artist": { "type": "string" }, "track_id": { "type": "string" }, "download_url": { "type": "string" }, "preview_url": { "type": "string" }, "license": { "type": "string" }, "license_url": { "type": "string" }, "page_url": { "type": "string" } }),
             vec![]),
        tool("klipp_match_grade",
             "Match one shot's grade to another's. It reads a frame of each clip through the \
              scopes and sets the target's exposure, lift, gain, temperature and tint to bring \
              its black, mid and white points and its colour balance toward the reference — the \
              continuity grade, so shots that sit next to each other look like one scene. The \
              frames are secs and reference_secs when given (timeline seconds, inside each \
              clip), else the middle of each clip's longest detected shot — so a clip with a \
              cut in it is read inside one shot, not across the cut. A first pass, applied as \
              one undo step; verify with klipp_check color or klipp_frame_stats and refine \
              with grade. Both must be picture clips.",
             json!({ "clip_id": { "type": "string", "description": "the shot to grade" }, "reference_clip_id": { "type": "string", "description": "the shot to match it to" }, "secs": { "type": "number", "description": "the target frame to read, timeline seconds inside the clip" }, "reference_secs": { "type": "number", "description": "the reference frame to read, timeline seconds inside it" } }),
             vec!["clip_id", "reference_clip_id"]),
        tool("klipp_align_audio",
             "Line a clip's sound up with another's — dual-system sound, a separate recorder \
              against the camera's scratch track. It cross-correlates the two clips' audio, \
              finds the offset, and moves clip_id so it plays in sync with reference_clip_id; \
              it reports the match confidence and refuses when the two share no common sound. \
              max_offset_secs bounds the search (30 by default). Both clips must carry audio.",
             json!({ "clip_id": { "type": "string", "description": "the clip to move into sync (e.g. the external audio)" }, "reference_clip_id": { "type": "string", "description": "the clip to sync to (e.g. the camera)" }, "max_offset_secs": { "type": "number" } }),
             vec!["clip_id", "reference_clip_id"]),
        tool("klipp_looks",
             "The bundled looks — 3D LUTs — an agent can apply, by category: cinematic and film \
              emulations (B&W, slide, negative, print, instant, Fuji). Read it to see the names \
              klipp_look takes; pass a category to see all of its looks. A creative choice a \
              piece asks for, on top of the grade, never a substitute for grading to the numbers.",
             json!({ "category": { "type": "string", "description": "a category to list in full, e.g. cinematic or film-print" } }), vec![]),
        tool("klipp_look",
             "Put a creative look on a picture clip: a bundled 3D LUT, chosen by name (see \
              klipp_looks), as one node after the grade so it sits on the corrected image. name \
              is category/name or any words that pick one out; opacity (0-1, default 1) fades it \
              in; remove clears it. Applied as one undo step. Grade for exposure and continuity \
              first, then a look for style. See it with frame_at. One look over a whole piece or \
              a scene: on \"timeline\" puts it on the timeline grade, after every clip's own; on \
              {group} puts it on a grade group's post-clip grade (make the group with the \
              grade_group op) — one call, one undo step, and every clip follows a later change.",
             json!({ "clip_id": { "type": "string" }, "on": { "description": "instead of clip_id: \"timeline\", or {\"group\": name, \"stage\": \"post\" (default) or \"pre\"}" }, "name": { "type": "string", "description": "a look name from klipp_looks, or an absolute .cube path" }, "opacity": { "type": "number" }, "remove": { "type": "boolean" } }),
             vec![]),
        tool("klipp_orient",
             "Start here. One call for where you are: the build, the project and whether it is \
              saved, the brief, every media item with its analysis state, the sequence so far, \
              and — in \"next\" — what to do now. Call it first, and again whenever you are \
              unsure of the state.",
             json!({}), vec![]),
        tool("klipp_brief",
             "The brief: what the piece is for. Its length, platform, picture size, loudness, \
              whether it is captioned and has music, and the cast with names and roles as \
              they should appear on screen. Read it before you plan; if there is none, ask \
              the user for what it should say and set it.",
             json!({}), vec![]),
        tool("klipp_set_brief",
             "Replace the brief, as one undo step; with merge true, change only the fields \
              given and keep the rest (a field given as null is cleared, a name the brief \
              does not have is refused). A brief is an object with any of: title, \
              type_styles (the brand's type system: [{name, font?, bold?, size?, color?, \
              fill?, backing?, full_frame?, x?, y?, align?, outline?, outline_color?, glow?, \
              glow_color?, gradient?, animate?, animate_secs?, stagger?, enter?, exit?, loop_effect?}] — a \
              title op or a plan title names one by style and its own fields override; enter/exit use the title op motion object, including kind none to disable; \
              heading, callout and card are built in and a style of that name replaces it), \
              about (what the piece is about), tone, platform, target_secs, width and height \
              (the delivery's picture), loudness_lufs (about -14 for the web, -23 for \
              broadcast), captions (bool), music (bool), cast (a list of {name, role, \
              described}), features (a list of {title, description, rank}: what the piece \
              shows off, 1 the headline — the edit plan's segments name these), arc (its \
              shape: \"hook, three features, call to action\"), cta (what the viewer is asked \
              to do), brand_colors ([\"#rrggbb\", ...], the first is what titles take), \
              look ({name} for a preset — broadcast_neutral, web_punchy, film_soft, \
              warm_interview, cool_tech, screencast — or the numbers black_l, white_l, mid_l, \
              contrast_range, chroma_mean, cast_a, cast_b, skin_hue; style_profile lifts a \
              look_target from a reference to paste here), \
              vocabulary (names, products and terms as they are spelt, for the \
              speech-to-text: run klipp_analyze after setting it), notes. What is left out asks for nothing. Names in lower thirds \
              come from the cast, klipp_check holds the piece to the brief, and an export \
              that names no size or loudness takes the brief's.",
             json!({ "brief": { "type": "object" }, "merge": { "type": "boolean", "description": "change only the fields given; the rest stay" } }), vec!["brief"]),
        tool("klipp_edit_plan",
             "The edit plan: the paper edit, as text and as JSON, with whether it is approved and \
              the ops it lowers to. Read it to show the user what is proposed, or to see what \
              build_edit_plan would build.",
             json!({}), vec![]),
        tool("klipp_set_edit_plan",
             "Propose the paper edit, as one undo step: the cut said before a clip is placed, so \
              the user can read, change and approve it while changing it is cheap. `plan` is \
              {segments: [...], music?: {media_id, duck_db?}, notes?}. Each segment, in cut \
              order: {media_id, source_in_secs, source_out_secs (stills may hold past their import duration), feature? (a title from the \
              brief's features, as spelt there), note? (what it shows and why — write one), \
              placement? {x?, y?, width?, height?, rotation_deg?, opacity?} uses compose coordinates (equal width and height); background? {generator: solid|gradient|living_gradient|grain|circle|countdown|placeholder|color_bars, colors?: [#rrggbb, ...]} places a backdrop below the picture, taking the brief palette when colors are omitted; with_audio? (the file's own sound; off by default), title? {text, secs?, style? \
              heading|callout|card (card covers the shot by default), x? and y? as frame fractions, full_frame? (false overlays), color?, fill?, glow? 0-1, glow_color?, gradient? [top, \
              bottom], animate? fade|rise|drop|slide_left|slide_right|scale_up|scale_down|blur|\
              typewriter|mask_rise|mask_drop|pop|slam|fly_left|fly_right|fly_top|fly_bottom|bounce|flicker|focus_in|soft_rise, animate_secs?, stagger? char|word|line, enter?, exit? (the title op motion objects)}, transition? \
              (\"cross_dissolve\", \"dip_to_black\": how it joins the segment before), zoom? \
              {from?, to, x?, y?}, sfx? {media_id (from search_sfx), at_secs? into the \
              segment, source_in_secs?, secs?, volume_db?}}. Music is {media_id, duck_db?, \
              source_in_secs?, volume_db?, fade_in_secs?, fade_out_secs?}. With snap_to_bars \
              true the bed's beats are read and every segment is rounded to whole bars (half \
              bars under one) and the bed starts on its first beat; the reply says what moved. \
              Give consecutive montage segments section: \"montage\" (or \"montage: name\"). \
              These snap to beats instead of bars. Once built from an approved plan, their \
              screen reading minimum is relaxed and short titles use word-count timing. \
              A montage title exactly matching a brief feature counts as its label when \
              feature is omitted; an explicit feature takes precedence. \
              Ground the stretches in \
              klipp_beat_sheet: cut to a settled reveal and hold until it reads; put titles in \
              a hold window. The plan is stored unapproved; the reply says which of the brief's \
              features it leaves out. Show its text to the user and ask.",
             json!({ "plan": { "type": "object" } }), vec!["plan"]),
        tool("klipp_approve_edit_plan",
             "Mark the edit plan approved, as one undo step. Call it only after the user has \
              read the plan and said to build it — never on your own judgement. The user can \
              also approve and build it themselves in Klipp's Edit Plan window (Window menu).",
             json!({}), vec![]),
        tool("klipp_build_edit_plan",
             "Build the approved edit plan onto the timeline after whatever is down, as a dry \
              run by default — the reply says what it would build, step by step, and changes \
              nothing. With apply true it is built as one undo step (undo takes it all back). \
              Refused while the plan is unapproved. `detail` adds every step as JSON.",
             json!({ "apply": { "type": "boolean" }, "detail": { "type": "boolean" } }), vec![]),
        tool("klipp_check",
             "The check before delivery, as an editor would watch it once: black and flash \
              frames, jump cuts with nothing over the join, titles across a cut or too short \
              to read, captions under a lower third, sound cut inside a word, music left at \
              full level under speech, picture and sound ending apart, offline media, and \
              the brief: length, size, captions, cast, music. On a screen recording with a \
              beat sheet: a cut in or out while the screen is still changing, a screen too \
              short to read, a title over a changing screen (with the file's hold windows \
              to move it to), a flat cadence; a cut to a settle is not called a jump cut. \
              With an edit plan: the brief's features it leaves out or gives under three \
              seconds, and an approved plan not built or drifted from. Every finding says where, \
              what, and what would put it right. Changes nothing. Run it before klipp_export, \
              fix the errors, decide about the warnings. Pass color:true to also read a \
              frame of each shot on the top track through the scopes — clipped highlights, \
              crushed blacks, a dull picture, over-saturation, skin off the skin-tone line \
              — so the grade is checked to the numbers, not by eye. Slower: it renders frames.",
             json!({ "color": { "type": "boolean", "description": "also measure a frame of every shot through the scopes for grade faults (slower)" } }), vec![]),
        tool("klipp_rename_sequence",
             "Name the active sequence: what the timeline is called in the app and in the \
              project file. A new project's sequence has a default or inherited name until \
              it is named; the brief's title does not rename it.",
             json!({ "name": { "type": "string" } }), vec!["name"]),
        tool("klipp_create_multicam",
             "Make a multicam group from pool items (two or more angles of one event): synced \
              onto one clock and laid out at the playhead, each angle on tracks of its own, as \
              Create Synced Multicam does. sync is \"timecode\", \"audio\" or \
              \"timecode_then_audio\" (default: timecode where a clip has it, sound for the \
              rest). Seek first to where it should go. The sync runs in the background: read \
              klipp_status (its multicam entry) or klipp_timeline once it is laid out.",
             json!({
                 "media_ids": { "type": "array", "items": { "type": "string" }, "description": "the angles, from klipp_list_media" },
                 "sync": { "type": "string", "enum": ["timecode", "audio", "timecode_then_audio"] }
             }), vec!["media_ids"]),
        tool("klipp_switch_angle",
             "Cut the multicam group in play to an angle, counted from 1 as keys 1-9 are, at \
              at_secs or at the playhead: from there on the program shows that camera, until \
              the next cut. One undo step per cut. klipp_status's multicam entry lists the \
              group's angles, the one in effect and the cut list; klipp_timeline marks each \
              clip's angle and the primary's cuts. Cutting to a transcript: switch to the \
              speaker's angle at each change of speaker.",
             json!({
                 "angle": { "type": "integer", "minimum": 1 },
                 "at_secs": { "type": "number", "description": "timeline seconds; the playhead when left out" }
             }), vec!["angle"]),
        tool("klipp_set_multicam_sound",
             "Choose whose sound the multicam group at at_secs (or the playhead) plays. Each \
              angle brings its own sound, and heard together they echo, so a group plays one: \
              an angle counted from 1 (the default is angle 1, the sound a synced group was \
              matched to; pick the lav or the camera nearest the speaker), \"follow\" to cut \
              the sound with the picture, or \"off\" when a recorder's track outside the \
              group carries the sound. Playback and export hear the same. klipp_status's \
              multicam entry says which is set.",
             json!({
                 "sound": { "description": "an angle number from 1, \"follow\" or \"off\"" },
                 "at_secs": { "type": "number", "description": "timeline seconds; the playhead when left out" }
             }), vec!["sound"]),
        tool("klipp_add_marker",
             "Put a marker on the timeline: how you and the editor leave each other notes. \
              Mark what you are unsure of or want looked at, with a label and a note saying \
              why, and the editor jumps through them. at_secs is timeline seconds; \
              duration_secs makes it a range. color: red, orange, yellow, green, blue, \
              purple; kind: chapter, todo, comment, best_take, section (chapters export as \
              chapter points). Answers with the marker's id.",
             json!({
                 "at_secs": { "type": "number" },
                 "duration_secs": { "type": "number" },
                 "label": { "type": "string" },
                 "note": { "type": "string", "description": "the longer comment" },
                 "color": { "type": "string", "enum": ["red", "orange", "yellow", "green", "blue", "purple"] },
                 "kind": { "type": "string", "enum": ["chapter", "todo", "comment", "best_take", "section"] }
             }), vec!["at_secs"]),
        tool("klipp_list_markers",
             "Every marker on the timeline in time order, with its id, time, range, label, \
              note, colour and kind: the editor's notes to you, and yours to them. Read them \
              at the start of a session: a todo marker is work asked for.",
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
             "Start an empty project: name, and location an existing folder (the project is \
              <location>/<name>.klipp, saved at once). width, height and fps set its first \
              sequence; left out, both follow the first clip placed. Refused when the open \
              project has unsaved changes unless discard_changes is true: klipp_save_project \
              first.",
             json!({
                 "name": { "type": "string" },
                 "location": { "type": "string", "description": "absolute path of an existing folder" },
                 "width": { "type": "integer" },
                 "height": { "type": "integer" },
                 "fps": { "type": "number", "description": "25, 29.97, 30, 59.94 …" },
                 "sequence_name": { "type": "string" },
                 "discard_changes": { "type": "boolean" }
             }), vec!["name", "location"]),
        tool("klipp_new_sequence",
             "Add a sequence to the project and open it, at a picture size and rate: a vertical \
              1080x1920 at 30 for a phone cut beside the 16:9 master, a 1080x1080 square. \
              The media pool is shared; place clips on it as on any sequence.",
             json!({
                 "name": { "type": "string" },
                 "width": { "type": "integer" },
                 "height": { "type": "integer" },
                 "fps": { "type": "number" }
             }), vec!["name", "width", "height", "fps"]),
        tool("klipp_save_project",
             "Save the project: to an absolute path (a folder ending in .klipp, made if \
              needed), or where it was last saved when no path is given. Save before a long \
              piece of work and when you are done, so the user can carry on by hand. \
              klipp_status says where the project is saved and whether it has unsaved changes.",
             json!({ "path": { "type": "string" } }), vec![]),
        tool("klipp_relink_media",
             "Point a media item at a file: an offline item found again, or a better copy. \
              The file is judged against what the project knew of the item (reel, start \
              timecode, length, rate, size) and relinked; the answer's check says how sure \
              that is. A file that is probably not the clip is refused with the reason; \
              force: true relinks it anyway (a relink is not undone by undo). \
              klipp_list_media marks items offline.",
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
             "The whole Klipp window as the person sees it, panels and dialogs included, as a \
              PNG (1600 wide by default): for checking a layout, a panel or a dialog. The \
              viewer in it is drawn through its monitor LUT and overlays, so measure colour \
              with klipp_screenshot or klipp_frame_stats, never from this.",
             json!({ "path": { "type": "string" }, "max_width": { "type": "integer" } }), vec![]),
        tool("klipp_open_project",
             "Open the project at an absolute path, replacing the one that is open. Refused \
              while the open project has unsaved changes: save it first, or pass \
              discard_changes true to lose them on purpose.",
             json!({ "path": { "type": "string" }, "discard_changes": { "type": "boolean" } }), vec!["path"]),
        tool("klipp_frame_stats",
             "The scope readings of the frame at a time, the numbers a colourist grades to: \
              the black, mid and white luma points in IRE, the RGB balance and any cast, the \
              saturation, how much is clipped at white and black — and in CIELAB: lightness \
              percentiles, contrast, chroma, the cast on the neutrals (a*, b*, named), and \
              every face read as a grid of skin patches (hue against the 40–65° skin band, \
              chroma, lightness, spread), each judged against the brief's look with the knob \
              that fixes it. Read it before and after a grade — grade to the numbers, not by \
              eye, and match a shot to its neighbours by them.",
             json!({ "secs": { "type": "number" }, "timeout_ms": { "type": "integer" } }), vec!["secs"]),
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
        tool("klipp_scope",
             "A scope image of a frame, GPU-drawn the way the app's own scopes are: the \
              waveform, the RGB parade, the vectorscope, or the histogram, as a PNG. It reads \
              the frame at secs (the playhead when omitted) without disturbing the scopes the \
              viewer is looking at. The colourist's second eye; frame_stats gives the same \
              frame as numbers to grade to.",
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
mod interface_tests {
    use super::*;

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
        let queue = tool_to_command("klipp_analysis_queue", &json!({ "media_id": "take", "cancel_pending": true })).unwrap();
        assert_eq!(queue["media_id"], "take");
        assert_eq!(queue["cancel_pending"], true);
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
